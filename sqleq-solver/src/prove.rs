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
        // The frontend's own lowering of `SELECT CAST(ts AS DATE)` vs `SELECT ts` over a TIMESTAMP
        // column: DATE and TIMESTAMP both read INTEGER in the IR, but truncating a timestamp to a
        // day changes it.
        let cast = project_one(json!({ "operator": "CAST", "type": "INTEGER", "operand": [col(1)] }));
        let v = verify(&json!({ "schemas": schema(), "queries": [cast, project_one(col(1))] }));
        assert_eq!(v, Verdict::NotProved(NotProvedReason::Exhausted));
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
