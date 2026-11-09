// Unless explicitly stated otherwise all files in this repository are licensed under the
// Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/).
// Copyright 2026-Present Datadog, Inc.

//! The decision ladder: what `IrDriver.java` + `Verification.verify` do for one job row, ported rung
//! by rung. Tier 0 (identical IR trees) answers first; then both sides are translated, checked for
//! equal arity and equal output column types, normalized with and without the integrity
//! constraints, and compared up to renaming of bound variables (rung 2), then by the set solver
//! (rung 3), and last by the bag congruence rung, which has no counterpart in Java.

use std::time::{Duration, Instant};

use crate::solver::alpha::alpha_eq;
use crate::solver::bagsolver;
use crate::solver::ic::Ics;
use crate::solver::ir::{Input, TranslateError};
use crate::solver::normalize::Normalizer;
use crate::solver::opaque;
use crate::solver::setsolver;
use crate::solver::smt::Facts;
use crate::solver::translate::{translate_input, Query};
use crate::solver::uterm::UTerm;

/// Terms bigger than this (read as trees, see [`crate::solver::uterm::UTerm::tree_size`]) are not rewritten.
/// Rewriting passes recurse over the tree, so their cost is its size, not the shared size, and one
/// pathological pair must not stall a batch. Terms that large come from long chains of outer
/// joins; a pair whose two sides are identical never gets here, because tier 0 answers it first.
pub const MAX_TREE_SIZE: usize = 1_000_000;

/// How long a pair's normalization may run, both attempts together. Past it, normalization stops
/// between rounds with the term it has -- every intermediate term is equivalent to the input -- and
/// the rungs compare that, so a pair that would normalize past the driver's cap answers instead.
/// The proofs found on the benchmark corpus take well under half of it; the pairs that outgrow it
/// are long chains of outer joins, some under a correlated subquery.
pub const NORMALIZE_BUDGET: Duration = Duration::from_secs(25);

/// How long the attempts with the squash lift may normalize, both together, within
/// [`NORMALIZE_BUDGET`]. The terms the lift proves (a filtered `GROUP BY`, a key filter moved past
/// an aggregate) normalize in a fraction of it; a term that takes longer is one whose plain
/// normalization was long already, and doing it again with the lift is all the extra time costs.
pub const LIFT_BUDGET: Duration = Duration::from_secs(1);

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    /// Proved equal. `literal` means tier 0 answered (the two IR trees are identical), which is a
    /// syntactic coincidence, never to be counted as a proof -- same flag `IrDriver` records.
    Eq { literal: bool },
    /// Translated, but no rung proved equality. Not a disproof: Java's `NEQ` means the same thing
    /// (see `LogicSupport.java:245` -- it is what a failed proof attempt returns).
    NotProved(NotProvedReason),
    /// Parse or translation refused the pair (Java's `NOTRANS`).
    Refused(TranslateError),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NotProvedReason {
    /// The two sides return different numbers of columns (`LogicSupport.java:221,239`).
    ArityMismatch,
    /// The two sides' output columns differ in type, or one's type is not known. Not in Java: its
    /// translation keeps the types, ours keeps only the values, and a value alone does not say
    /// its type -- `TRUE` and `1` are both the term `1`.
    TypeMismatch,
    /// A term is larger than [`MAX_TREE_SIZE`].
    TooLarge,
    /// Every rung ran and none proved equality.
    Exhausted,
}

impl std::fmt::Display for Verdict {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Verdict::Eq { literal: true } => f.write_str("EQ literal"),
            Verdict::Eq { literal: false } => f.write_str("EQ"),
            Verdict::NotProved(NotProvedReason::ArityMismatch) => f.write_str("NEQ arity-mismatch"),
            Verdict::NotProved(NotProvedReason::TypeMismatch) => f.write_str("NEQ type-mismatch"),
            Verdict::NotProved(NotProvedReason::TooLarge) => f.write_str("NEQ too-large"),
            Verdict::NotProved(NotProvedReason::Exhausted) => f.write_str("NEQ"),
            Verdict::Refused(e) => write!(f, "NOTRANS {e}"),
        }
    }
}

// Tier 0 sits beside the IR reader, which sqleq-cvc5 shares without the rest of this module.
pub use crate::ir::identical_sides;

/// One job row's `ir` value, end to end: tier 0 ([`identical_sides`]), then [`prove`].
pub fn verify(ir: &serde_json::Value) -> Verdict {
    if identical_sides(ir) {
        return Verdict::Eq { literal: true };
    }
    match Input::parse(ir) {
        Ok(input) => prove(&input),
        Err(e) => Verdict::Refused(e),
    }
}

/// The prover proper, for a pair tier 0 did not answer.
pub fn prove(input: &Input) -> Verdict {
    prove_at(input, 0)
}

/// [`prove`] for a pair met `depth` sub-proofs deep: the sources of two slices compared by
/// [`opaque`], which first replaces the nodes translation cannot read by tables shared between the
/// two sides, where it can.
pub fn prove_at(input: &Input, depth: usize) -> Verdict {
    let abstracted = opaque::abstract_opaque(input, depth);
    let input = abstracted.as_ref().unwrap_or(input);
    let [l, r] = match translate_input(input) {
        Ok(sides) => sides,
        Err(e) => return Verdict::Refused(e),
    };
    decide(&l, &r, &Ics::from_schemas(&input.schemas), &Facts::from_schemas(&input.schemas))
}

fn decide(l: &Query, r: &Query, ics: &Ics, facts: &Facts) -> Verdict {
    if l.arity != r.arity {
        return Verdict::NotProved(NotProvedReason::ArityMismatch);
    }
    // Equal terms are equal values only of one type: a boolean is the 0/1 a predicate is, so
    // `SELECT TRUE` and `SELECT 1` are one term, and an output whose type the IR leaves unresolved
    // matches nothing.
    if l.types != r.types || l.types.iter().any(Option::is_none) {
        return Verdict::NotProved(NotProvedReason::TypeMismatch);
    }
    if l.term.tree_size(MAX_TREE_SIZE + 1) > MAX_TREE_SIZE || r.term.tree_size(MAX_TREE_SIZE + 1) > MAX_TREE_SIZE {
        return Verdict::NotProved(NotProvedReason::TooLarge);
    }
    // Sound as far as it goes: both sides number their bound vars from 0 in translation order and
    // share only the output var, so structurally identical terms denote the same function of it.
    if l.term == r.term {
        return Verdict::Eq { literal: false };
    }
    // Rung 2, with the integrity constraints and then without (`LogicSupport.java:231-247`): the
    // constraint rewrites are not monotone for provability -- they can restructure one side in a
    // way the other does not follow -- so a failure with them is not a failure without them.
    let mut too_large = false;
    let none = Ics::default();
    let attempts: Vec<&Ics> = if ics.is_empty() { vec![&none] } else { vec![ics, &none] };
    let start = Instant::now();
    let normalize_by = start + NORMALIZE_BUDGET;
    let mut plain = Vec::with_capacity(attempts.len());
    for ics in &attempts {
        let Some(sides) = normalize_pair(l, r, ics, normalize_by, false) else {
            too_large = true;
            plain.push(None);
            continue;
        };
        if rungs(&sides, facts, Some(start + bagsolver::BUDGET)) {
            return Verdict::Eq { literal: false };
        }
        plain.push(Some(sides));
    }
    // Then the same attempts with the factors every summand of a squash holds lifted out of it
    // (`normalize::lift_squashes`), which is what lets a `GROUP BY` key a filter holds constant pin
    // the group's output column. Only after the normal form without it has failed: it moves factors
    // in ways the other side may not follow, so it is tried beside that form, never instead of it,
    // and a pair proved without it is proved as before. Only where it changed a side, and only the
    // rungs that compare normal forms (alpha-equivalence and the set solver), not the bag rung's
    // search: a lifted key that pins a group's column makes the two sides one normal form, which
    // is what those rungs see, and the bag rung has already had its time on the forms above.
    let lift_by = normalize_by.min(Instant::now() + LIFT_BUDGET);
    for (ics, plain) in attempts.iter().zip(&plain) {
        let Some(sides) = normalize_pair(l, r, ics, lift_by, true) else {
            too_large = true;
            continue;
        };
        // A lift of closed factors alone pins nothing, so it is not worth the rungs' time.
        if !sides.lifted_open || plain.as_ref().is_some_and(|p| p.terms() == sides.terms()) {
            continue;
        }
        if rungs(&sides, facts, None) {
            return Verdict::Eq { literal: false };
        }
    }
    Verdict::NotProved(if too_large { NotProvedReason::TooLarge } else { NotProvedReason::Exhausted })
}

/// Both sides normalized with `ics`, and with the squash lift when `lift`, with the widths each
/// normalizer ended with. `None` when a side outgrows the size budget.
struct Sides {
    l: (UTerm, std::collections::HashMap<u32, usize>),
    r: (UTerm, std::collections::HashMap<u32, usize>),
    /// Whether a squash lift moved a factor that mentions a var, on either side.
    lifted_open: bool,
}

impl Sides {
    fn terms(&self) -> (&UTerm, &UTerm) {
        (&self.l.0, &self.r.0)
    }
}

fn normalize_pair(l: &Query, r: &Query, ics: &Ics, normalize_by: Instant, lift: bool) -> Option<Sides> {
    let side = |q: &Query| {
        let n = Normalizer::new(q.widths.clone(), MAX_TREE_SIZE).with_ics(ics.clone()).with_deadline(normalize_by);
        let mut n = if lift { n.with_squash_lift() } else { n };
        let t = n.normalize(&q.term).ok()?;
        Some((t, n.lifted_open(), n.widths))
    };
    let ((tl, ol, wl), (tr, or, wr)) = (side(l)?, side(r)?);
    Some(Sides { l: (tl, wl), r: (tr, wr), lifted_open: ol || or })
}

/// Compare two normalized sides up to renaming of bound vars (rung 2), and failing that ask the set
/// solver (rung 3), as `SqlSolver.proveEq` does, and then the bag congruence rung, which has no
/// counterpart there, until `bag_by` (not at all when `None`). The row facts hold on every database
/// the claims are about, so they are given to the last rung on every attempt.
fn rungs(s: &Sides, facts: &Facts, bag_by: Option<Instant>) -> bool {
    let ((tl, wl), (tr, wr)) = (&s.l, &s.r);
    alpha_eq(tl, wl, tr, wr)
        || setsolver::prove(tl, wl, tr, wr) == Some(true)
        || bag_by.is_some_and(|by| bagsolver::prove(tl, wl, tr, wr, facts, by))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn schema() -> serde_json::Value {
        json!([{ "types": ["INTEGER", "INTEGER"], "key": [], "nullable": [true, true] }])
    }

    #[test]
    fn identical_ir_is_answered_by_tier_0_even_when_it_would_be_refused() {
        // A Sort with a LIMIT is refused by translation, but tier 0 never gets that far.
        let q = json!({ "sort": { "source": { "scan": 0 }, "collation": [], "limit": 1 } });
        assert_eq!(verify(&json!({ "schemas": schema(), "queries": [q.clone(), q] })), Verdict::Eq { literal: true });
    }

    #[test]
    fn different_arities_are_not_proved() {
        let narrow = json!({ "project": { "source": { "scan": 0 }, "target": [{ "column": 0, "type": "INTEGER" }] } });
        let v = verify(&json!({ "schemas": schema(), "queries": [{ "scan": 0 }, narrow] }));
        assert_eq!(v, Verdict::NotProved(NotProvedReason::ArityMismatch));
    }

    fn project_one(target: serde_json::Value) -> serde_json::Value {
        json!({ "project": { "source": { "scan": 0 }, "target": [target] } })
    }

    fn col(i: u32) -> serde_json::Value {
        json!({ "column": i, "type": "INTEGER" })
    }

    #[test]
    fn a_join_reads_a_derived_table_on_its_right_by_the_tables_own_columns() {
        // `SELECT s.id FROM s JOIN (SELECT * FROM t WHERE t.<k> = 1) AS d ON TRUE` against the same
        // derived table on the left of the join, with `s(id, t_id)` and `t(id, x, c)`, as the
        // frontend lowers them: either way the filter names `t`'s columns from the enclosing base.
        let schemas = json!([
            { "name": "s", "types": ["INTEGER", "INTEGER"], "key": [[0]], "nullable": [false, true] },
            { "name": "t", "types": ["INTEGER", "INTEGER", "INTEGER"], "key": [[0]], "nullable": [false, true, true] },
        ]);
        let one = json!({ "operator": "1", "operand": [], "type": "INTEGER" });
        let on_true = json!({ "operator": "TRUE", "operand": [], "type": "BOOLEAN" });
        let t_where = |k: u32| json!({ "filter": { "source": { "scan": 1 }, "condition": cmp("=", col(k), one.clone()) } });
        let select = |left: serde_json::Value, right: serde_json::Value, s_id: u32| {
            json!({ "project": { "target": [col(s_id)], "source": { "join": {
                "kind": "INNER", "condition": on_true.clone(), "left": left, "right": right } } } })
        };
        let on_right = |k: u32| select(json!({ "scan": 0 }), t_where(k), 0);
        let on_left = |k: u32| select(t_where(k), json!({ "scan": 0 }), 3);
        let pair = |a: serde_json::Value, b: serde_json::Value| verify(&json!({ "schemas": schemas, "queries": [a, b] }));
        // Filtering `t.c` is not filtering `t.id`, though `t.c` on the right sits where `t.id`
        // would if the right input were numbered from after `s`.
        assert!(!matches!(pair(on_right(2), on_left(0)), Verdict::Eq { .. }));
        // The inputs of an inner join commute.
        assert_eq!(pair(on_right(2), on_left(2)), Verdict::Eq { literal: false });
    }

    #[test]
    fn a_widening_cast_under_division_blocks_the_proof() {
        // Integer division truncates and real division does not, so these differ (7/2 vs 7.0/2).
        let real_div = project_one(json!({ "operator": "/", "type": "REAL", "operand": [
            { "operator": "CAST", "type": "REAL", "operand": [col(0)] }, col(1),
        ]}));
        let int_div = project_one(json!({ "operator": "/", "type": "INTEGER", "operand": [col(0), col(1)] }));
        // Their output types differ as well, which now settles it first.
        let v = verify(&json!({ "schemas": schema(), "queries": [real_div, int_div] }));
        assert_eq!(v, Verdict::NotProved(NotProvedReason::TypeMismatch));
    }

    #[test]
    fn a_cast_between_equal_ir_types_is_not_the_identity() {
        // The frontend drops a cast it knows is the identity before emitting, so one that survives
        // is not. (It once lowered `CAST(ts AS DATE)` this way, with both types read as INTEGER.)
        let cast = project_one(json!({ "operator": "CAST", "type": "INTEGER", "operand": [col(1)] }));
        let v = verify(&json!({ "schemas": schema(), "queries": [cast, project_one(col(1))] }));
        assert_eq!(v, Verdict::NotProved(NotProvedReason::Exhausted));
    }

    /// `t(ts TIMESTAMP, d DATE)`, filtered on `cond`, projecting column 0 -- the shape the frontend
    /// emits for the temporal pairs below, conversions included.
    fn temporal(cond: serde_json::Value) -> serde_json::Value {
        json!({ "project": { "source": { "filter": { "source": { "scan": 0 }, "condition": cond } },
                             "target": [{ "column": 0, "type": "TIMESTAMP" }] } })
    }

    fn cmp(op: &str, l: serde_json::Value, r: serde_json::Value) -> serde_json::Value {
        json!({ "operator": op, "type": "BOOLEAN", "operand": [l, r] })
    }

    fn conv(name: &str, ty: &str, x: serde_json::Value) -> serde_json::Value {
        json!({ "operator": name, "type": ty, "operand": [x] })
    }

    #[test]
    fn the_frontends_temporal_conversions_are_functions_not_identities() {
        let schema = json!([{ "types": ["TIMESTAMP", "DATE"], "key": [], "nullable": [true, true] }]);
        let ts = || json!({ "column": 0, "type": "TIMESTAMP" });
        let d = || json!({ "column": 1, "type": "DATE" });
        let d_plus_1 = json!({ "operator": "+", "type": "DATE", "operand": [d(), { "operator": "1", "operand": [], "type": "INTEGER" }] });
        // `ts < d + 1` vs `ts <= d`: different for any mid-day `ts`.
        let a = temporal(cmp("<", ts(), conv("q_conv_date_timestamp", "TIMESTAMP", d_plus_1)));
        let b = temporal(cmp("<=", ts(), conv("q_conv_date_timestamp", "TIMESTAMP", d())));
        let v = verify(&json!({ "schemas": schema, "queries": [a, b] }));
        assert_eq!(v, Verdict::NotProved(NotProvedReason::Exhausted));
        // `CAST(ts AS DATE) = d` vs `ts = d`: the truncation is not the identity.
        let a = temporal(cmp("=", conv("q_conv_timestamp_date", "DATE", ts()), d()));
        let b = temporal(cmp("=", ts(), conv("q_conv_date_timestamp", "TIMESTAMP", d())));
        let v = verify(&json!({ "schemas": schema, "queries": [a, b] }));
        assert_eq!(v, Verdict::NotProved(NotProvedReason::Exhausted));
        // The same conversion on both sides, reordered, still matches.
        let a = temporal(cmp("=", conv("q_conv_timestamp_date", "DATE", ts()), d()));
        let b = temporal(cmp("=", d(), conv("q_conv_timestamp_date", "DATE", ts())));
        let v = verify(&json!({ "schemas": schema, "queries": [a, b] }));
        assert_eq!(v, Verdict::Eq { literal: false });
    }

    #[test]
    fn a_negated_order_comparison_is_its_complement_under_set_semantics() {
        // `SELECT DISTINCT a FROM t WHERE NOT (b < 1)` against `... WHERE b >= 1`, as the frontend
        // lowers them (DISTINCT as a keys-only group). Both are UNKNOWN on a NULL `b`; otherwise
        // they agree, the values of one type being totally ordered.
        let one = || json!({ "operator": "1", "operand": [], "type": "INTEGER" });
        let not = |e| json!({ "operator": "NOT", "type": "BOOLEAN", "operand": [e] });
        let distinct = |cond| json!({ "group": { "function": [], "keys": [col(0)], "source": { "project": {
            "source": { "filter": { "source": { "scan": 0 }, "condition": cond } },
            "target": [col(0)] } } } });
        let pair = |a, b| verify(&json!({ "schemas": schema(), "queries": [distinct(a), distinct(b)] }));
        assert_eq!(pair(not(cmp("<", col(1), one())), cmp(">=", col(1), one())), Verdict::Eq { literal: false });
        assert_eq!(pair(not(cmp(">", col(1), one())), cmp("<=", col(1), one())), Verdict::Eq { literal: false });
        // Strict and non-strict stay apart: `b < 1` is not `b <= 1`.
        assert_eq!(pair(cmp("<", col(1), one()), cmp("<=", col(1), one())), Verdict::NotProved(NotProvedReason::Exhausted));
    }

    #[test]
    fn the_same_cast_on_both_sides_still_matches() {
        let cast = || project_one(json!({ "operator": "CAST", "type": "INTEGER", "operand": [col(1)] }));
        let filtered = json!({ "project": {
            "source": { "filter": { "source": { "scan": 0 }, "condition": { "operator": "true", "operand": [], "type": "BOOLEAN" } } },
            "target": [{ "operator": "CAST", "type": "INTEGER", "operand": [col(1)] }],
        }});
        let v = verify(&json!({ "schemas": schema(), "queries": [cast(), filtered] }));
        assert_eq!(v, Verdict::Eq { literal: false });
    }

    #[test]
    fn structurally_identical_translations_of_different_ir_are_proved() {
        // A bare ORDER BY is erased by translation, so these differ as IR but not as terms. Sound
        // under the bag semantics sqleq decides (`src/fuzz/mod.rs:6-7`: an ORDER BY-only
        // difference never counts) -- stricter than Java, whose `OrderbySupport` compares orderings.
        let sorted = json!({ "sort": { "source": { "scan": 0 }, "collation": [[0, "INTEGER", "ASCENDING NULLS LAST"]] } });
        let v = verify(&json!({ "schemas": schema(), "queries": [{ "scan": 0 }, sorted] }));
        assert_eq!(v, Verdict::Eq { literal: false });
    }
}
