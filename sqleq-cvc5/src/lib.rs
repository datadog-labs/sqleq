// Unless explicitly stated otherwise all files in this repository are licensed under the
// Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/).
// Copyright 2026-Present Datadog, Inc.

//! The `cvc5` axis: the lowered `Input` IR as an SMT problem over cvc5's theory of bags (or sets)
//! of tuples, which is unsatisfiable exactly when the two queries are equivalent under the
//! encoding, and whose model, when there is one, is a database on which they differ.
//!
//! It follows the Java translator of the LPAR'24 relations work (`Cvc5AbstractTranslator`,
//! `Cvc5BagsTranslator`, `Cvc5SetsTranslator` in the `spes` repository), operator for operator,
//! but reads this repository's IR instead of Calcite plans: `ir` from `sqleq-solver`, the same
//! reader that crate proves from, so a plan means one thing to both axes. A table is a constant of
//! sort `(Bag (Tuple ...))` -- `(Set (Tuple ...))` under set semantics -- with every column
//! `Nullable`; a projection is `table.project` or `bag.map`, a `WHERE` is `bag.filter`, a join is
//! `table.product` under a filter, `UNION ALL` is `bag.union_disjoint`, `DISTINCT` is
//! `bag.setof`, a `GROUP BY` is a `table.aggr` fold, and SQL's three-valued logic is lifted over
//! the nullable columns. `docs/CVC5.md` lays out the encoding and what it approximates.
//!
//! Two answers are claims. `unsat` is a proof of equivalence under the encoding; `sat` comes with
//! the model's tables, a database on which the two encodings differ. Unlike SQLSolver's `NEQ`,
//! that is evidence: not an executed counterexample, as sqleq-fuzz produces, but an instance the
//! solver checked against its own encoding (`check-models`), whose faithfulness to Postgres is
//! what `docs/CVC5.md` bounds. The binary, `sqleq-cvc5`, speaks `IrDriver`'s command line and
//! job/result JSONL with that one addition, a `model` on a `NEQ` row.

pub mod translate;

pub use translate::{check, smt2, Failure, Model, Options, Outcome, Refusal, Semantics};
