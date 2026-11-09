// Unless explicitly stated otherwise all files in this repository are licensed under the
// Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/).
// Copyright 2026-Present Datadog, Inc.

//! Rung 3: the set solver (SQLSolver's `SetSolver.java`).
//!
//! Where rung 2 compares two normalized terms syntactically, this rung asks Z3 whether they can
//! differ, for terms whose every summation sits under a squash or negation: there the U-expression
//! is plain first-order logic -- a sum is `∨`, a product `∧`, `Σ` is `∃` -- so the question is
//! decidable enough to hand to a solver. `¬(A = B)` UNSAT means the two agree everywhere.
//!
//! Unlike Java, applicability is not "translation happened not to throw": Java refuses every term
//! with a parameter carrier, a column inside a function, or an `IS NULL`, which rules out most real
//! queries. The encoding (one uninterpreted sort for values, tables as functions to multiplicities,
//! sums as `∃`) is [`crate::solver::smt`]'s, read without its opt-in extensions.

use std::collections::HashMap;

use z3::{Params, SatResult, Solver};

use crate::solver::smt::{Encoder, Side};
use crate::solver::uterm::{UConst, UTerm, UVar};

/// Z3's own per-query limit. Running out is "not proved", which is sound.
pub const TIMEOUT_MS: u32 = 3_000;

/// Whether rung 3 applies: every `Sum` in a multiplicity position sits under a `Squash`/`Neg`, and
/// no `Sum` sits in a value position (an aggregate or scalar-subquery value).
pub fn applicable(t: &UTerm) -> bool {
    fn count(t: &UTerm, under_set: bool) -> bool {
        match t {
            UTerm::Const(UConst::Int(_)) | UTerm::Table { .. } => true,
            UTerm::Const(_) | UTerm::Var(_) | UTerm::Func { .. } => false,
            UTerm::Pred { args, .. } => args.iter().all(value),
            UTerm::Add(ts) | UTerm::Mul(ts) => ts.iter().all(|c| count(c, under_set)),
            UTerm::Squash(c) | UTerm::Neg(c) => count(c, true),
            UTerm::Sum { body, .. } => under_set && count(body, true),
        }
    }
    fn value(t: &UTerm) -> bool {
        match t {
            UTerm::Const(_) | UTerm::Var(UVar::Proj { .. }) | UTerm::Table { .. } => true,
            UTerm::Var(UVar::Base(_)) | UTerm::Sum { .. } => false,
            UTerm::Pred { args, .. } | UTerm::Func { args, .. } => args.iter().all(value),
            UTerm::Add(ts) | UTerm::Mul(ts) => ts.iter().all(|c| value(c)),
            UTerm::Squash(c) | UTerm::Neg(c) => count(c, true),
        }
    }
    count(t, false)
}

/// `Some(true)` when Z3 shows the two terms equal on every database; `Some(false)` when it does not
/// (a counter-model, a timeout, or `unknown` -- never a disproof); `None` when the rung does not
/// apply to one of them.
///
/// Terms live in the calling thread's Z3 context, which the binding creates implicitly, so the time
/// limit and seed are set on the solver rather than on a context.
pub fn prove(a: &UTerm, widths_a: &HashMap<u32, usize>, b: &UTerm, widths_b: &HashMap<u32, usize>) -> Option<bool> {
    if !applicable(a) || !applicable(b) {
        return None;
    }
    let mut enc = Encoder::new();
    let ea = enc.count(a, Side::A, widths_a)?;
    let eb = enc.count(b, Side::B, widths_b)?;
    let solver = Solver::new();
    let mut params = Params::new();
    params.set_u32("timeout", TIMEOUT_MS);
    params.set_u32("random_seed", 9876);
    solver.set_params(&params);
    for fact in enc.background() {
        solver.assert(&fact);
    }
    solver.assert(ea.eq(&eb).not());
    Some(solver.check() == SatResult::Unsat)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::solver::translate::OUT_VAR_ID;
    use crate::solver::uterm::PredKind;
    use std::rc::Rc;

    fn col(i: u32, v: u32) -> UTerm {
        UTerm::Var(UVar::proj(i, UVar::Base(v)))
    }

    fn eq(a: UTerm, b: UTerm) -> UTerm {
        UTerm::Pred { kind: PredKind::Eq, args: vec![a, b] }
    }

    fn mul(fs: Vec<UTerm>) -> UTerm {
        UTerm::Mul(fs.into_iter().map(Rc::new).collect())
    }

    fn exists(v: u32, body: UTerm) -> UTerm {
        UTerm::Squash(Rc::new(UTerm::Sum { vars: vec![UVar::Base(v)], body: Rc::new(body) }))
    }

    fn table(name: &str, v: u32) -> UTerm {
        UTerm::Table { name: name.into(), var: UVar::Base(v) }
    }

    fn k(n: i64) -> UTerm {
        UTerm::Const(UConst::Int(n))
    }

    #[test]
    fn a_bag_sum_at_the_top_is_not_applicable() {
        let t = UTerm::Sum { vars: vec![UVar::Base(0)], body: Rc::new(table("r", 0)) };
        assert!(!applicable(&t));
        assert!(applicable(&exists(0, table("r", 0))));
    }

    #[test]
    fn a_condition_implied_inside_an_exists_is_proved_redundant() {
        // T(out)·‖∃y S(y)·[y.0 = out.0]·[out.0 = 1]‖  vs  T(out)·‖∃y S(y)·[y.0 = 1]·[out.0 = 1]‖
        let out = OUT_VAR_ID;
        let a = mul(vec![table("t", out), exists(0, mul(vec![table("s", 0), eq(col(0, 0), col(0, out)), eq(col(0, out), k(1))]))]);
        let b = mul(vec![table("t", out), exists(0, mul(vec![table("s", 0), eq(col(0, 0), k(1)), eq(col(0, out), k(1))]))]);
        let w = HashMap::from([(0, 1), (out, 1)]);
        assert_eq!(prove(&a, &w, &b, &w), Some(true));
    }

    #[test]
    fn different_constants_are_not_confused() {
        let out = OUT_VAR_ID;
        let a = mul(vec![table("t", out), eq(col(0, out), k(1))]);
        let b = mul(vec![table("t", out), eq(col(0, out), k(2))]);
        let w = HashMap::from([(out, 1)]);
        assert_eq!(prove(&a, &w, &b, &w), Some(false));
    }
}
