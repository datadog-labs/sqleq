// Unless explicitly stated otherwise all files in this repository are licensed under the
// Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/).
// Copyright 2026-Present Datadog, Inc.

//! `sqleq-check` -- a batch harness that runs SQL equivalence pairs past every axis.
//!
//! It takes `.sql` pair files (or directories of them), runs each through the frontend (SQL ->
//! the QED prover's `Input` JSON) and then the QED prover, and produces a consolidated,
//! CI-friendly report. Each input `.sql` file declares its table schemas and functions and holds
//! **exactly two** statements; the question is whether the two are provably equivalent under bag
//! semantics.
//!
//! The taxonomy says `refused` rather than `parse_error`: the frontend declines constructs it
//! cannot lower *faithfully*, which is a deliberate soundness choice and not the same event as a
//! parse failure, so the two are distinguished by `refuse_kind` on each case.
//!
//! Every report is split into **trivial** and **non-trivial** pairs. A pair is trivial when its
//! two queries reach the prover identical -- the frontend's own (equivalence-preserving)
//! normalizations collapsed the difference, so the prover is confirming `x = x`. Proving those is
//! sound but not evidence of capability, and in practice they are often the large majority, so the
//! summary prints `capability` -- proved among the pairs that actually differ -- directly beneath
//! the raw `proved` count.
//!
//! `--sqleq-solver` adds a **second opinion** on the same cases, run over the very `Input` JSON the
//! QED prover is handed: sqleq-solver, this repo's Rust rewrite of SQLSolver's proof engine, or with
//! `--sqlsolver-jvm` instead the original SQLSolver through the bridge in `tools/sqlsolver/`, kept
//! as a backup cross-check of sqleq-solver. It never changes the exit code -- the qed axis decides
//! the policy -- because it answers a different question. Two things must be read off it carefully,
//! and the summary says both:
//!
//! * **That prover never disproves.** Its `NEQ` means "no proof found", exactly like its `UNKNOWN`;
//!   only its `EQ` is a claim. A case we prove and it calls `NEQ` is not a contradiction, and
//!   nothing here may treat it as one. sqleq-fuzz is the only disprover in this project.
//! * **The two opinions share a frontend, so they are not independent.** A lowering bug yields the
//!   same wrong plan on both axes; agreement corroborates the provers, not the frontend.
//!
//! `--portfolio` asks every backend at once on each case instead, within that case's one deadline,
//! and reports one combined verdict -- `equivalent`, `not-equivalent`, `alarm` when a proof meets a
//! counterexample, `timeout` or `undecided` -- which then decides the exit code; see [`portfolio`].
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
