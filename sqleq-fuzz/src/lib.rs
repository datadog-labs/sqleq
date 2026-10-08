// Unless explicitly stated otherwise all files in this repository are licensed under the
// Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/).
// Copyright 2026-Present Datadog, Inc.

//! `sqleq-fuzz` — a license-clean concrete differential tester: a SQL *non-equivalence* disprover.
//!
//! For a query pair `(A, B)` under a schema, it generates small **valid** random database instances
//! (honouring NOT NULL and every UNIQUE / PRIMARY KEY / UNIQUE INDEX, empty tables included), binds
//! `$N` params to random typed values consistently across A and B, holds `now()`/`current_*` to one
//! instant per trial and skips nondeterministic functions, runs both statements on PostgreSQL ([`pg`]) — or,
//! with `--engine duckdb`, on DuckDB, as DuckDB evaluates them ([`test_pair`]) — and
//! compares outputs as **sorted multisets** (bag semantics — an ORDER BY-only difference never
//! counts). SELECT pairs compare result sets; UPDATE/DELETE/INSERT pairs compare final table
//! state, and the returned rows too when both sides carry RETURNING. A pair with no one observable to
//! compare — a query against a mutation, RETURNING on one side only, an EXPLAIN — is
//! `NOT-COMPARABLE` instead. Any difference on a valid, deterministic instance is a **sound
//! counterexample** ⇒ the pair is non-equivalent, under the semantics of the engine that found it:
//! Postgres's for the Postgres engine, DuckDB's for the DuckDB one.
//!
//! Binding `$N` to one value for the pair is an assumption, not a given — the row does not record which
//! of A's placeholders the application fills from the same value as which of B's. Where the two queries
//! mention different sets of `$N`, that assumption is visibly wrong and no verdict is available in
//! either direction: substituting one value compares two queries the caller never paired, so a
//! difference is not a counterexample and an agreement is not evidence. Those pairs report
//! `PARAM-MISALIGNED` ([`patterns::misalignment`], [`test_pair`]).
//!
//! Every soundness rule a verdict rests on is enforced here rather than assumed of the caller:
//! uniqueness enforcement, time freezing, LIMIT/OFFSET handling, array canonicalization, the
//! cardinality-only rule, and value-from-data param biasing. `README.md` states each one.

pub mod duck;
pub mod gen;
pub mod lex;
pub mod limits;
pub mod pair;
pub mod patterns;
pub mod pg;
pub mod rewrite;
pub mod schema;
pub mod typing;

pub use pair::{test_pair, Config, Verdict};
