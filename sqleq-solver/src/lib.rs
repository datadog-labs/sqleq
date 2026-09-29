// Unless explicitly stated otherwise all files in this repository are licensed under the
// Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/).
// Copyright 2026-Present Datadog, Inc.

//! Rust port of SQLSolver's equivalence-checking proof engine, a native alternative to the JVM
//! driver the `sqlsolver` axis runs through (`tools/sqlsolver/`). The binary, `sqleq-solver`,
//! speaks `IrDriver`'s command line and job/result JSONL, so a harness can swap one for the other.
//!
//! Deliberately excluded: SQLSolver's superoptimizer/rule-mining subsystem and all SQL-text
//! parsing -- neither is reachable from the `Verification.verify(RelNode, RelNode, Schema)` entry
//! point our harness actually calls, and our own frontend already produces the `Input` JSON this
//! crate will deserialize instead of SQL text.
//!
//! The pipeline, one module per stage: `ir` parses the `Input` JSON; `translate` turns each side
//! into a U-expression (`uterm`), with SQL's three-valued logic kept explicit; `normalize` rewrites
//! both sides toward a canonical sum-of-products form, using the schemas' integrity constraints
//! (`ic`); `alpha` compares them up to renaming of bound variables (SQLSolver's rung 2);
//! `setsolver` hands set-shaped leftovers to Z3 (rung 3); `prove` is the ladder over all of it.
//! `eval` evaluates a term on a small concrete database, which is how the tests check that a
//! rewrite preserves meaning. SQLSolver's LIA* rung is not ported. The pairs it would add need
//! integer reasoning across the conversions between dates and timestamps; the IR keeps those types
//! apart and names every conversion (`q_conv_date_timestamp` and the like), which is what such a
//! rung would have to interpret, and the commonest of those pairs the frontend already lowers to
//! one term.

pub mod alpha;
pub mod eval;
pub mod ic;
pub mod ir;
pub mod normalize;
pub mod prove;
pub mod setsolver;
pub mod translate;
pub mod uterm;

#[cfg(test)]
mod smoke_tests {
    use std::time::{Duration, Instant};
    use z3::ast::{Ast, Int};
    use z3::{Config, Context, SatResult, Solver};

    /// Z3 links against the configured `libz3.so` and answers a trivial query.
    #[test]
    fn z3_links_and_answers() {
        let cfg = Config::new();
        let ctx = Context::new(&cfg);
        let solver = Solver::new(&ctx);

        // x + 1 == x is unsat for any integer x.
        let x = Int::new_const(&ctx, "x");
        let one = Int::from_i64(&ctx, 1);
        solver.assert(&(&x + &one)._eq(&x));
        assert_eq!(solver.check(), SatResult::Unsat);

        // x == 5 is sat, and the model should say so.
        solver.reset();
        let five = Int::from_i64(&ctx, 5);
        solver.assert(&x._eq(&five));
        assert_eq!(solver.check(), SatResult::Sat);
        let model = solver.get_model().expect("sat query must produce a model");
        assert_eq!(model.eval(&x, true).and_then(|v| v.as_i64()), Some(5));
    }

    /// The `timeout` config param is wired up and doesn't hang the process. This is a smoke test of
    /// the mechanism, not a proof it always bites in time; the driver's per-row cap is the backstop.
    #[test]
    fn timeout_param_does_not_hang() {
        let mut cfg = Config::new();
        cfg.set_timeout_msec(1);
        let ctx = Context::new(&cfg);
        let solver = Solver::new(&ctx);

        let x = Int::new_const(&ctx, "x");
        let y = Int::new_const(&ctx, "y");
        let zero = Int::from_i64(&ctx, 0);
        solver.assert(&x.gt(&zero));
        solver.assert(&y.gt(&zero));
        solver.assert(&(&x * &y)._eq(&Int::from_i64(&ctx, 1_000_003 * 1_000_033)));

        let start = Instant::now();
        let result = solver.check();
        assert!(
            start.elapsed() < Duration::from_secs(5),
            "a 1ms configured timeout should not let this run for seconds"
        );
        // Either answer is acceptable here (Z3 may solve it before the timeout fires); the
        // property under test is that the call returns promptly, not which SatResult comes back.
        let _ = result;
    }
}
