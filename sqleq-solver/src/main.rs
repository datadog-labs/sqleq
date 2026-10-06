// Unless explicitly stated otherwise all files in this repository are licensed under the
// Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/).
// Copyright 2026-Present Datadog, Inc.

//! `sqleq-solver`: this crate behind `IrDriver`'s command line and I/O contract, so a harness runs
//! it or the JVM driver the same way and changes nothing else.
//!
//! ```text
//! sqleq-solver <ir.jobs.jsonl> <results.jsonl> [--timeout-ms=N] [--grace-ms=N] [--dry-run]
//! ```
//!
//! Each job line is `{name, ir, schema, refusal?}`; each result line, appended and flushed per row,
//! is `{name, verdict, ms, killed, literal?, refused?, error?}` with `IrDriver`'s vocabulary:
//! `NOIR` (the frontend built no IR), `EQ` (`literal: true` when the two IR trees are identical,
//! tier 0, which is never a proof), `NEQ` (no proof found -- never a disproof), `UNKNOWN`,
//! `NOTRANS` (the IR could not be translated), `ERROR`, `HANG`, and `TRANSLATED` under `--dry-run`.
//! `IrDriver` also writes `TIMEOUT`; this binary never does. Progress goes to stderr as
//! `name verdict Nms`.
//!
//! The per-row cap is enforced by running each row on its own thread. A row that answers after the
//! cap but within `--grace-ms` keeps its verdict, with `killed: true`. A thread cannot be killed,
//! so a row still running `--grace-ms` after the cap is written as `HANG` and the process exits
//! with status 3 -- exactly `IrDriver`'s self-halt, which callers already answer by resuming on a
//! fresh process over the rows not yet written. Z3's own per-query timeout is the primary cap; this
//! is the backstop.
//!
//! A plan nested deeper than [`ir::MAX_DEPTH`] is answered `NOTRANS` (`nesting-too-deep`) without
//! being read, and the row's thread has a stack sized for every stage at that depth: a stack
//! overflow cannot be caught, and it would abort the whole run.

use std::io::{BufRead, Write};
use std::panic::{self, AssertUnwindSafe};
use std::process::ExitCode;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use sqleq_solver::ir::{self, Input, TranslateError};
use sqleq_solver::prove::{self, NotProvedReason, Verdict};
use sqleq_solver::translate::translate_input;

const USAGE: &str = "usage: sqleq-solver <ir.jobs.jsonl> <results.jsonl> [--timeout-ms=N] [--grace-ms=N] [--dry-run]";

#[derive(Serialize)]
struct Row {
    name: String,
    verdict: &'static str,
    ms: u128,
    killed: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    literal: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    refused: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}

/// The fields of a job line read before its plan is: the plan itself is skipped without recursing
/// into it, so a pathologically deep one can still be named in a refusal.
#[derive(Deserialize)]
struct Header {
    name: Option<String>,
}

/// The stack each row's thread runs on. Translation, normalization and the solver's encoding all
/// recurse over the plan; at [`ir::MAX_DEPTH`] they need a few tens of megabytes, far past a
/// spawned thread's default of 2 MiB. It is address space, reserved, not memory used.
const WORKER_STACK: usize = 512 << 20;

/// The JSON nesting depth of `text` (objects and arrays), counted without parsing it.
fn text_depth(text: &str) -> usize {
    let (mut depth, mut deepest, mut in_string, mut escaped) = (0usize, 0usize, false, false);
    for b in text.bytes() {
        if in_string {
            match b {
                _ if escaped => escaped = false,
                b'\\' => escaped = true,
                b'"' => in_string = false,
                _ => {}
            }
            continue;
        }
        match b {
            b'"' => in_string = true,
            b'{' | b'[' => {
                depth += 1;
                deepest = deepest.max(depth);
            }
            b'}' | b']' => depth = depth.saturating_sub(1),
            _ => {}
        }
    }
    deepest
}

/// What the worker thread reports back.
enum Outcome {
    Verdict(Verdict),
    Translated,
    Panic(String),
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.len() < 2 {
        eprintln!("{USAGE}");
        return ExitCode::from(2);
    }
    let (mut cap_ms, mut grace_ms, mut dry) = (60_000u64, 5_000u64, false);
    for a in &args[2..] {
        if let Some(v) = a.strip_prefix("--timeout-ms=") {
            cap_ms = v.parse().unwrap_or_else(|_| usage_error(a));
        } else if let Some(v) = a.strip_prefix("--grace-ms=") {
            grace_ms = v.parse().unwrap_or_else(|_| usage_error(a));
        } else if a == "--dry-run" {
            dry = true;
        } else {
            usage_error(a);
        }
    }

    let jobs = match std::fs::File::open(&args[0]) {
        Ok(f) => std::io::BufReader::new(f),
        Err(e) => {
            eprintln!("cannot read {}: {e}", args[0]);
            return ExitCode::from(2);
        }
    };
    let mut out = match std::fs::OpenOptions::new().create(true).append(true).open(&args[1]) {
        Ok(f) => f,
        Err(e) => {
            eprintln!("cannot write {}: {e}", args[1]);
            return ExitCode::from(2);
        }
    };

    for line in jobs.lines() {
        let line = match line {
            Ok(l) => l,
            Err(e) => {
                eprintln!("cannot read {}: {e}", args[0]);
                return ExitCode::from(2);
            }
        };
        if line.trim().is_empty() {
            continue;
        }
        // The job object adds one level to its plan's.
        if text_depth(&line) > ir::MAX_DEPTH + 1 {
            let (row, _) = match serde_json::from_str::<Header>(&line) {
                Ok(h) => too_deep(h.name.unwrap_or_else(|| "?".to_string())),
                Err(e) => {
                    eprintln!("malformed job line in {}: {e}", args[0]);
                    return ExitCode::from(2);
                }
            };
            if write_row(&mut out, &row).is_err() {
                eprintln!("cannot write {}", args[1]);
                return ExitCode::from(2);
            }
            continue;
        }
        // Real IR nests past serde_json's default 128-level limit; the check above bounds it.
        let mut de = serde_json::Deserializer::from_str(&line);
        de.disable_recursion_limit();
        let job: serde_json::Value = match serde::de::Deserialize::deserialize(&mut de) {
            Ok(j) => j,
            Err(e) => {
                // A malformed job file is the caller's bug, and a row without a name could never
                // be matched on resume; stop rather than write one.
                eprintln!("malformed job line in {}: {e}", args[0]);
                return ExitCode::from(2);
            }
        };
        let name = job.get("name").and_then(|n| n.as_str()).unwrap_or("?").to_string();
        let (row, hung) = run(name, job, dry, Duration::from_millis(cap_ms), Duration::from_millis(grace_ms));
        if write_row(&mut out, &row).is_err() {
            eprintln!("cannot write {}", args[1]);
            return ExitCode::from(2);
        }
        if hung {
            eprintln!("driver: {} did not stop within the grace period; exiting so the harness resumes on a fresh process", row.name);
            return ExitCode::from(3);
        }
    }
    ExitCode::SUCCESS
}

/// Appends one result row, flushed, and reports it on stderr.
fn write_row(out: &mut std::fs::File, row: &Row) -> std::io::Result<()> {
    let text = serde_json::to_string(row).expect("a result row always serializes");
    writeln!(out, "{text}").and_then(|_| out.flush())?;
    eprintln!(
        "{} {} {}ms{}",
        row.name,
        row.verdict,
        row.ms,
        row.refused.as_deref().map(|r| format!(" refused={r}")).unwrap_or_default()
    );
    Ok(())
}

/// The row for a job whose plan is nested past [`ir::MAX_DEPTH`].
fn too_deep(name: String) -> (Row, bool) {
    let refused = Some(TranslateError::TooDeep.to_string());
    (Row { name, verdict: "NOTRANS", ms: 0, killed: false, literal: None, refused, error: None }, false)
}

fn usage_error(arg: &str) -> ! {
    eprintln!("unknown or malformed argument: {arg}\n{USAGE}");
    std::process::exit(2)
}

/// One job. The second value is whether the row's thread is still running (a `HANG`).
fn run(name: String, job: serde_json::Value, dry: bool, cap: Duration, grace: Duration) -> (Row, bool) {
    let t0 = Instant::now();
    let row = |verdict: &'static str| Row { name: name.clone(), verdict, ms: 0, killed: false, literal: None, refused: None, error: None };

    let ir = job.get("ir").cloned().unwrap_or(serde_json::Value::Null);
    if ir.is_null() {
        // The row exists even when the frontend declined it, so both drivers cover the same names.
        let refused = job.get("refusal").and_then(|r| r.as_str()).unwrap_or("frontend").to_string();
        return (Row { refused: Some(refused), ms: t0.elapsed().as_millis(), ..row("NOIR") }, false);
    }
    // Tier 0 on the raw IR, before anything is parsed: two identical trees are equal whatever the
    // prover would say. Recorded as `literal` because a syntactic coincidence is not a proof.
    if let Some([a, b]) = ir.get("queries").and_then(|q| q.as_array()).map(Vec::as_slice) {
        if a == b {
            return (Row { literal: Some(true), ms: t0.elapsed().as_millis(), ..row("EQ") }, false);
        }
    }

    let (tx, rx) = mpsc::channel();
    let spawned = std::thread::Builder::new().stack_size(WORKER_STACK).spawn(move || {
        let outcome = panic::catch_unwind(AssertUnwindSafe(|| {
            if dry {
                match Input::parse(&ir).and_then(|input| translate_input(&input).map(|_| ())) {
                    Ok(()) => Outcome::Translated,
                    Err(e) => Outcome::Verdict(Verdict::Refused(e)),
                }
            } else {
                Outcome::Verdict(match Input::parse(&ir) {
                    Ok(input) => prove::prove(&input),
                    Err(e) => Verdict::Refused(e),
                })
            }
        }))
        .unwrap_or_else(|payload| {
            let msg = payload
                .downcast_ref::<&str>()
                .map(|s| s.to_string())
                .or_else(|| payload.downcast_ref::<String>().cloned())
                .unwrap_or_else(|| "non-string panic payload".to_string());
            Outcome::Panic(msg)
        });
        // The receiver is gone only when the row was abandoned; nothing is waiting for it then.
        let _ = tx.send(outcome);
    });
    if let Err(e) = spawned {
        let error = Some(format!("cannot start the row's thread: {e}"));
        return (Row { error, ms: t0.elapsed().as_millis(), ..row("ERROR") }, false);
    }

    let (outcome, killed) = match rx.recv_timeout(cap) {
        Ok(o) => (Some(o), false),
        Err(_) => (rx.recv_timeout(grace).ok(), true),
    };
    let ms = t0.elapsed().as_millis();
    let mut r = match outcome {
        None => return (Row { killed: true, ms, ..row("HANG") }, true),
        Some(Outcome::Translated) => row("TRANSLATED"),
        Some(Outcome::Panic(msg)) => Row { error: Some(format!("panic: {msg}")), ..row("ERROR") },
        Some(Outcome::Verdict(v)) => match v {
            // Tier 0 already answered above, so an EQ here is the prover's own.
            Verdict::Eq { literal } => Row { literal: Some(literal), ..row("EQ") },
            Verdict::NotProved(NotProvedReason::TooLarge) => row("UNKNOWN"),
            Verdict::NotProved(_) => row("NEQ"),
            Verdict::Refused(e) => Row { refused: Some(e.to_string()), ..row("NOTRANS") },
        },
    };
    r.ms = ms;
    r.killed = killed;
    (r, false)
}
