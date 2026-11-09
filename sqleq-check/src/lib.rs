// Unless explicitly stated otherwise all files in this repository are licensed under the
// Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/).
// Copyright 2026-Present Datadog, Inc.

//! `sqleq-check` -- the command-line entry point, which runs SQL equivalence pairs past the axes
//! asked for.
//!
//! It takes `.sql` pair files (or directories of them), runs each through the frontend (SQL -> the
//! lowered `Input` JSON) and then sqleq-solver, this repo's prover, and produces a consolidated,
//! CI-friendly report. Each input `.sql` file may declare table schemas and functions, and holds
//! **exactly two** statements; the question is whether the two are equivalent under bag semantics.
//!
//! A case the frontend will not lower is `refused`, and `refuse_kind` says why: the frontend
//! declines constructs it cannot lower *faithfully*, which is a deliberate soundness choice and
//! not the same event as a parse failure, so the two are told apart (`unsupported` against
//! `parse`).
//!
//! Every report is split into **trivial** and **non-trivial** pairs. A pair is trivial when its
//! two queries reach the prover identical -- the frontend's own (equivalence-preserving)
//! normalizations collapsed the difference, so the prover is confirming `x = x`. Proving those is
//! sound but not evidence of capability, and such pairs can make up much of a corpus, so the
//! summary prints `capability` -- proved among the pairs that actually differ -- directly beneath
//! the raw `proved` count.
//!
//! sqleq-solver is a Rust rewrite of SQLSolver's proof engine, and the prover the default
//! `--expect equivalent` policy reads. `--qed` adds the QED prover as a **second opinion** on the
//! same cases, over the very `Input` JSON sqleq-solver is handed; with `--axes` leaving sqleq-solver
//! out, the QED prover is the run's prover instead. `--sqlsolver-jvm` asks the original SQLSolver
//! through the bridge in `tools/sqlsolver/` in sqleq-solver's place, a deprecated backup cross-check
//! that never decides the policy. Outside `--portfolio` and `--expect pinned` a second opinion never
//! changes the exit code, because it answers a different question. Two things must be read off the
//! provers carefully, and the summary says both:
//!
//! * **A prover's no-proof is not a disproof.** sqleq-solver's `NEQ` means "no proof found",
//!   exactly like its `UNKNOWN`, and the QED prover's `unprovable` is the same; only a proof is a
//!   claim. A case one proves and the other does not is not a contradiction, and nothing here may
//!   treat it as one. sqleq-fuzz is the only disprover in this project.
//! * **The two opinions share a frontend, so they are not independent.** A lowering bug yields the
//!   same wrong plan on both axes; agreement corroborates the provers, not the frontend.
//!
//! `--cvc5` adds the **cvc5 axis** on the same plan: `sqleq-cvc5` encodes it over cvc5's bags (or
//! sets) of tuples, after the LPAR'24 relations work's Java translator. Its `proved` is a proof
//! under that encoding, and its `model` -- a database on which the two encodings differ -- a
//! refutation of the plan: an alarm against a proof over the same plan, but no verdict on its own,
//! because the plan lacks the constraints the frontend does not carry and the encoding reads the
//! IR's types its own way (`docs/CVC5.md`). sqleq-fuzz's executed counterexample remains the
//! refutation of the pair, and the report keeps the two words apart. The axis is experimental: its
//! proof decides nothing either -- no portfolio verdict, no exit code, no pinned pair's truth --
//! and is reported, and an alarm against a counterexample, only.
//!
//! `--portfolio` asks every backend at once on each case instead, within that case's one deadline,
//! and reports one combined verdict -- `equivalent`, Lean's weaker `equivalent-gather` and
//! `equivalent-gather-generated`, `not-equivalent`, sqleq-fuzz's `not-equivalent-gather`, `alarm`
//! when a proof meets a counterexample under one binding, `timeout` or `undecided` -- which then
//! decides the exit code; see [`portfolio`].
//!
//! `--corpus` runs the rows of a corpus CSV instead of pair files, each through every backend's own
//! corpus code, with what a long run needs beside it: one JSON line per case as it finishes and
//! `--resume`, memory caps, a second retry tier, and parallel solver drivers.
//!
//! `--expect pinned` is the other policy: every case carries its own expected answer per axis in
//! its header (`tests/pairs/README.md`), `--axes` picks which axes run -- sqleq-fuzz among them --
//! and any movement fails. The grammar and the judgement live in [`suite`].

pub mod app;
pub mod axes;
pub mod case;
pub mod cli;
pub mod discover;
pub mod inputs;
pub mod pinned;
pub mod portfolio;
pub mod proc;
pub mod report;
pub mod suite;
pub mod util;
