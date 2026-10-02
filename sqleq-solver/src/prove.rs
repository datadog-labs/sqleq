// Unless explicitly stated otherwise all files in this repository are licensed under the
// Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/).
// Copyright 2026-Present Datadog, Inc.

//! The decision ladder: what `IrDriver.java` + `Verification.verify` do for one job row, ported rung
//! by rung. Tier 0 (identical IR trees) answers first; then both sides are translated, checked for
//! equal arity, normalized with and without the integrity constraints, and compared up to renaming
//! of bound variables (rung 2), with the set solver as the last resort (rung 3).

use crate::alpha::alpha_eq;
use crate::ic::Ics;
use crate::ir::{Input, TranslateError};
use crate::normalize::Normalizer;
use crate::setsolver;
use crate::translate::{translate_input, Query};

/// Terms bigger than this (read as trees, see [`crate::uterm::UTerm::tree_size`]) are not rewritten.
/// Rewriting passes recurse over the tree, so their cost is its size, not the shared size, and one
/// pathological pair must not stall a batch. Terms that large come from long chains of outer
/// joins; a pair whose two sides are identical never gets here, because tier 0 answers it first.
pub const MAX_TREE_SIZE: usize = 1_000_000;

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
            Verdict::NotProved(NotProvedReason::TooLarge) => f.write_str("NEQ too-large"),
            Verdict::NotProved(NotProvedReason::Exhausted) => f.write_str("NEQ"),
            Verdict::Refused(e) => write!(f, "NOTRANS {e}"),
        }
    }
}

/// One job row's `ir` value, end to end. Tier 0 runs on the raw JSON before anything is parsed,
/// exactly as `IrDriver.java:128-136` does, so an identical pair is answered even when its shape
/// would be refused.
pub fn verify(ir: &serde_json::Value) -> Verdict {
    if let Some([a, b]) = ir.get("queries").and_then(|q| q.as_array()).map(Vec::as_slice) {
        if a == b {
            return Verdict::Eq { literal: true };
        }
    }
    match Input::parse(ir) {
        Ok(input) => prove(&input),
        Err(e) => Verdict::Refused(e),
    }
}

/// The prover proper, for a pair tier 0 did not answer.
pub fn prove(input: &Input) -> Verdict {
    let [l, r] = match translate_input(input) {
        Ok(sides) => sides,
        Err(e) => return Verdict::Refused(e),
    };
    decide(&l, &r, &Ics::from_schemas(&input.schemas))
}

fn decide(l: &Query, r: &Query, ics: &Ics) -> Verdict {
    if l.arity != r.arity {
        return Verdict::NotProved(NotProvedReason::ArityMismatch);
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
    for ics in attempts {
        match rung2(l, r, ics) {
            Some(true) => return Verdict::Eq { literal: false },
            Some(false) => {}
            None => too_large = true,
        }
    }
    Verdict::NotProved(if too_large { NotProvedReason::TooLarge } else { NotProvedReason::Exhausted })
}

/// Normalize each side, then compare up to renaming of bound vars (rung 2), and failing that ask
/// the set solver (rung 3), as `SqlSolver.proveEq` does. `None` when a side outgrows the size
/// budget.
fn rung2(l: &Query, r: &Query, ics: &Ics) -> Option<bool> {
    let mut nl = Normalizer::new(l.widths.clone(), MAX_TREE_SIZE).with_ics(ics.clone());
    let mut nr = Normalizer::new(r.widths.clone(), MAX_TREE_SIZE).with_ics(ics.clone());
    let tl = nl.normalize(&l.term).ok()?;
    let tr = nr.normalize(&r.term).ok()?;
    if alpha_eq(&tl, &nl.widths, &tr, &nr.widths) {
        return Some(true);
    }
    Some(setsolver::prove(&tl, &nl.widths, &tr, &nr.widths) == Some(true))
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
        let v = verify(&json!({ "schemas": schema(), "queries": [real_div, int_div] }));
        assert_eq!(v, Verdict::NotProved(NotProvedReason::Exhausted));
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
        // under the bag semantics sqleq decides (`sqleq-fuzz/src/lib.rs:6-7`: an ORDER BY-only
        // difference never counts) -- stricter than Java, whose `OrderbySupport` compares orderings.
        let sorted = json!({ "sort": { "source": { "scan": 0 }, "collation": [[0, "INTEGER", "ASCENDING NULLS LAST"]] } });
        let v = verify(&json!({ "schemas": schema(), "queries": [{ "scan": 0 }, sorted] }));
        assert_eq!(v, Verdict::Eq { literal: false });
    }
}
