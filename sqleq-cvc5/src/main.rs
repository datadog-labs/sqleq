// Unless explicitly stated otherwise all files in this repository are licensed under the
// Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/).
// Copyright 2026-Present Datadog, Inc.

//! `sqleq-cvc5`: the cvc5 encoding behind `IrDriver`'s command line and I/O contract, so a harness
//! drives it exactly as it drives `sqleq-solver`.
//!
//! ```text
//! sqleq-cvc5 <ir.jobs.jsonl> <results.jsonl> [--timeout-ms=N] [--grace-ms=N] [--semantics=bags|sets]
//!            [--opt=NAME=VALUE]... [--no-check-models] [--smt2=DIR] [--dry-run]
//! sqleq-cvc5 --plan <input.json> [--name=N] [the same options]
//! ```
//!
//! Each job line is `{name, ir, ...}` -- the job `sqleq-frontend --sqlsolver` packages, or just a
//! name and an `Input`; `schema` and the rest are ignored, since the encoding reads the plan's own
//! `schemas`. Each result line, appended and flushed per row, is `{name, verdict, ms, killed,
//! literal?, refused?, error?, model?, counterexample?, semantics}` in `IrDriver`'s vocabulary:
//! `EQ` for `unsat` (`literal: true` when the two IR trees are identical, tier 0, which is never a
//! proof), `NEQ` for `sat` -- with the model's tables as `model`, one line of them as
//! `counterexample`, and `model_exact`, true when the problem had no uninterpreted symbol and the
//! model is therefore a database and nothing more -- `UNKNOWN` for cvc5's `unknown`, `NOTRANS` when the plan was not
//! encoded (`refused` says why, in `sqleq-solver`'s spelling where the reason is the IR reader's),
//! `ERROR` when a cvc5 call failed, `HANG`, and `TRANSLATED` under `--dry-run`. Progress goes to
//! stderr as `name verdict Nms`.
//!
//! `--timeout-ms` is cvc5's `tlimit-per`, the budget of the one `check-sat`, which cvc5 enforces
//! itself. A row still running `--grace-ms` after it is written as `HANG` and the process exits
//! with status 3 -- `IrDriver`'s self-halt, which a caller answers by resuming on a fresh process
//! over the rows not yet written. `--semantics` picks bags (the default: SQL's own) or sets; each
//! `--opt` is a further cvc5 `set-option`, `--opt=bags-to-liastar=true` say; `--smt2=DIR` also
//! writes each row's problem as `DIR/<name>.smt2`, for re-checking with the `cvc5` binary.
//!
//! `--plan` takes one `Input` JSON instead of a jobs file and prints the row to stdout.

use std::io::{BufRead, Write};
use std::panic::{self, AssertUnwindSafe};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use sqleq_cvc5::{check, smt2, Failure, Model, Options, Outcome, Semantics};
use sqleq_solver::ir::{self, Input};
use sqleq_solver::prove::identical_sides;

const USAGE: &str = "usage: sqleq-cvc5 <ir.jobs.jsonl> <results.jsonl> [--timeout-ms=N] [--grace-ms=N] [--semantics=bags|sets] \
[--opt=NAME=VALUE]... [--no-check-models] [--smt2=DIR] [--dry-run]
       sqleq-cvc5 --plan <input.json> [--name=N] [the same options]";

#[derive(Serialize)]
struct Row {
    name: String,
    verdict: &'static str,
    ms: u128,
    killed: bool,
    semantics: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    literal: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    refused: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
    /// cvc5's explanation of an `unknown`.
    #[serde(skip_serializing_if = "Option::is_none")]
    unknown: Option<String>,
    /// How the two queries' row shapes differ, on a `NEQ` that claims no model.
    #[serde(skip_serializing_if = "Option::is_none")]
    incomparable: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    model: Option<Model>,
    /// Whether the problem had no uninterpreted symbol, so that `model` is a database and nothing
    /// more; under one, the model also fixes that symbol's values, and is only a candidate.
    #[serde(skip_serializing_if = "Option::is_none")]
    model_exact: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    counterexample: Option<String>,
}

#[derive(Deserialize)]
struct Header {
    name: Option<String>,
}

/// The stack each row's thread runs on: translation recurses over the plan, and cvc5 recurses over
/// the terms it builds from it.
const WORKER_STACK: usize = 512 << 20;

struct Config {
    cap_ms: Option<u64>,
    grace_ms: u64,
    dry: bool,
    opts: Options,
    smt2_dir: Option<PathBuf>,
}

/// The JSON nesting depth of `text`, counted without parsing it.
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

enum Done {
    Outcome(Result<Outcome, Failure>),
    Translated,
    Panic(String),
}

fn usage_error(arg: &str) -> ! {
    eprintln!("unexpected argument {arg}\n{USAGE}");
    std::process::exit(2)
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut positional: Vec<String> = Vec::new();
    let mut cfg = Config { cap_ms: None, grace_ms: 5_000, dry: false, opts: Options::default(), smt2_dir: None };
    let mut plan: Option<String> = None;
    let mut name: Option<String> = None;
    for a in &args {
        if let Some(v) = a.strip_prefix("--timeout-ms=") {
            cfg.cap_ms = Some(v.parse().unwrap_or_else(|_| usage_error(a)));
        } else if let Some(v) = a.strip_prefix("--grace-ms=") {
            cfg.grace_ms = v.parse().unwrap_or_else(|_| usage_error(a));
        } else if let Some(v) = a.strip_prefix("--semantics=") {
            cfg.opts.semantics = Semantics::parse(v).unwrap_or_else(|| usage_error(a));
        } else if let Some(v) = a.strip_prefix("--opt=") {
            let (k, val) = v.split_once('=').unwrap_or_else(|| usage_error(a));
            cfg.opts.cvc5_options.push((k.to_string(), val.to_string()));
        } else if let Some(v) = a.strip_prefix("--smt2=") {
            cfg.smt2_dir = Some(PathBuf::from(v));
        } else if let Some(v) = a.strip_prefix("--plan=") {
            plan = Some(v.to_string());
        } else if let Some(v) = a.strip_prefix("--name=") {
            name = Some(v.to_string());
        } else if a == "--no-check-models" {
            cfg.opts.check_models = false;
        } else if a == "--dry-run" {
            cfg.dry = true;
        } else if a == "--plan" || a == "--name" {
            // Written with a space: the value is the next positional.
            positional.push(a.clone());
        } else if a.starts_with("--") {
            usage_error(a);
        } else {
            positional.push(a.clone());
        }
    }
    // `--plan FILE` and `--name N` with a space.
    let mut rest = Vec::new();
    let mut it = positional.into_iter();
    while let Some(p) = it.next() {
        match p.as_str() {
            "--plan" => plan = Some(it.next().unwrap_or_else(|| usage_error("--plan"))),
            "--name" => name = Some(it.next().unwrap_or_else(|| usage_error("--name"))),
            _ => rest.push(p),
        }
    }
    cfg.opts.timeout_ms = cfg.cap_ms;
    if let Some(d) = &cfg.smt2_dir {
        if let Err(e) = std::fs::create_dir_all(d) {
            eprintln!("cannot create {}: {e}", d.display());
            return ExitCode::from(2);
        }
    }

    if let Some(path) = plan {
        if !rest.is_empty() {
            usage_error(&rest[0]);
        }
        let text = match std::fs::read_to_string(&path) {
            Ok(t) => t,
            Err(e) => {
                eprintln!("cannot read {path}: {e}");
                return ExitCode::from(2);
            }
        };
        let ir: serde_json::Value = match serde_json::from_str(&text) {
            Ok(v) => v,
            Err(e) => {
                eprintln!("{path} is not an Input JSON: {e}");
                return ExitCode::from(2);
            }
        };
        let name = name.unwrap_or_else(|| {
            Path::new(&path).file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_else(|| "plan".into())
        });
        let job = serde_json::json!({ "name": name, "ir": ir });
        let (row, hung) = run(name, job, &cfg);
        println!("{}", serde_json::to_string_pretty(&row).expect("a result row always serializes"));
        return if hung { ExitCode::from(3) } else { ExitCode::SUCCESS };
    }

    if rest.len() != 2 {
        eprintln!("{USAGE}");
        return ExitCode::from(2);
    }
    let jobs = match std::fs::File::open(&rest[0]) {
        Ok(f) => std::io::BufReader::new(f),
        Err(e) => {
            eprintln!("cannot read {}: {e}", rest[0]);
            return ExitCode::from(2);
        }
    };
    let mut out = match std::fs::OpenOptions::new().create(true).append(true).open(&rest[1]) {
        Ok(f) => f,
        Err(e) => {
            eprintln!("cannot write {}: {e}", rest[1]);
            return ExitCode::from(2);
        }
    };
    for line in jobs.lines() {
        let line = match line {
            Ok(l) => l,
            Err(e) => {
                eprintln!("cannot read {}: {e}", rest[0]);
                return ExitCode::from(2);
            }
        };
        if line.trim().is_empty() {
            continue;
        }
        if text_depth(&line) > ir::MAX_DEPTH + 1 {
            let row = match serde_json::from_str::<Header>(&line) {
                Ok(h) => too_deep(h.name.unwrap_or_else(|| "?".to_string()), cfg.opts.semantics),
                Err(e) => {
                    eprintln!("malformed job line in {}: {e}", rest[0]);
                    return ExitCode::from(2);
                }
            };
            if write_row(&mut out, &row).is_err() {
                eprintln!("cannot write {}", rest[1]);
                return ExitCode::from(2);
            }
            continue;
        }
        let mut de = serde_json::Deserializer::from_str(&line);
        de.disable_recursion_limit();
        let job: serde_json::Value = match serde::de::Deserialize::deserialize(&mut de) {
            Ok(j) => j,
            Err(e) => {
                eprintln!("malformed job line in {}: {e}", rest[0]);
                return ExitCode::from(2);
            }
        };
        let name = job.get("name").and_then(|n| n.as_str()).unwrap_or("?").to_string();
        let (row, hung) = run(name, job, &cfg);
        if write_row(&mut out, &row).is_err() {
            eprintln!("cannot write {}", rest[1]);
            return ExitCode::from(2);
        }
        if hung {
            eprintln!("driver: {} did not stop within the grace period; exiting so the harness resumes on a fresh process", row.name);
            return ExitCode::from(3);
        }
    }
    ExitCode::SUCCESS
}

fn write_row(out: &mut std::fs::File, row: &Row) -> std::io::Result<()> {
    let text = serde_json::to_string(row).expect("a result row always serializes");
    writeln!(out, "{text}").and_then(|_| out.flush())?;
    eprintln!(
        "{} {} {}ms{}{}",
        row.name,
        row.verdict,
        row.ms,
        row.refused.as_deref().map(|r| format!(" refused={r}")).unwrap_or_default(),
        row.counterexample.as_deref().map(|c| format!(" {c}")).unwrap_or_default()
    );
    Ok(())
}

fn blank(name: String, verdict: &'static str, sem: Semantics) -> Row {
    Row {
        name,
        verdict,
        ms: 0,
        killed: false,
        semantics: sem.name(),
        literal: None,
        refused: None,
        error: None,
        unknown: None,
        incomparable: None,
        model: None,
        model_exact: None,
        counterexample: None,
    }
}

fn too_deep(name: String, sem: Semantics) -> Row {
    let mut row = blank(name, "NOTRANS", sem);
    row.refused = Some(ir::TranslateError::TooDeep.to_string());
    row
}

/// One job: tier 0 on the raw JSON, then the encoding and cvc5 on a thread of its own, waited for
/// up to the cap plus the grace period. Returns the row and whether the row hung.
fn run(name: String, job: serde_json::Value, cfg: &Config) -> (Row, bool) {
    let t0 = Instant::now();
    let sem = cfg.opts.semantics;
    let mut row = blank(name.clone(), "ERROR", sem);
    let ir = job.get("ir").cloned().unwrap_or(serde_json::Value::Null);
    if ir.is_null() {
        row.verdict = "NOIR";
        row.refused = job.get("refusal").and_then(|r| r.as_str()).map(str::to_string).or(Some("no plan".into()));
        row.ms = t0.elapsed().as_millis();
        return (row, false);
    }
    if identical_sides(&ir) {
        row.verdict = "EQ";
        row.literal = Some(true);
        row.ms = t0.elapsed().as_millis();
        return (row, false);
    }
    let input = match Input::parse(&ir) {
        Ok(i) => i,
        Err(e) => {
            row.verdict = "NOTRANS";
            row.refused = Some(e.to_string());
            row.ms = t0.elapsed().as_millis();
            return (row, false);
        }
    };
    let (tx, rx) = mpsc::channel();
    let opts = cfg.opts.clone();
    let dry = cfg.dry;
    let smt2_path = cfg.smt2_dir.as_ref().map(|d| d.join(format!("{}.smt2", slug(&name))));
    let worker = std::thread::Builder::new().stack_size(WORKER_STACK).spawn(move || {
        let done = panic::catch_unwind(AssertUnwindSafe(|| {
            if let Some(p) = &smt2_path {
                match smt2(&input, &opts) {
                    Ok(text) => {
                        if let Err(e) = std::fs::write(p, text) {
                            eprintln!("cannot write {}: {e}", p.display());
                        }
                    }
                    Err(Failure::Refused(_)) => {}
                    Err(Failure::Cvc5(e)) => eprintln!("{}: {e}", p.display()),
                }
            }
            if dry {
                return match smt2(&input, &opts) {
                    Ok(_) => Done::Translated,
                    Err(e) => Done::Outcome(Err(e)),
                };
            }
            Done::Outcome(check(&input, &opts))
        }));
        let done = done.unwrap_or_else(|p| {
            let msg = p
                .downcast_ref::<String>()
                .cloned()
                .or_else(|| p.downcast_ref::<&str>().map(|s| s.to_string()))
                .unwrap_or_else(|| "panic".into());
            Done::Panic(msg)
        });
        let _ = tx.send(done);
    });
    if let Err(e) = worker {
        row.error = Some(format!("cannot start a worker thread: {e}"));
        row.ms = t0.elapsed().as_millis();
        return (row, false);
    }
    let budget = cfg.cap_ms.map(|c| Duration::from_millis(c + cfg.grace_ms));
    let got = match budget {
        None => rx.recv().ok(),
        Some(b) => rx.recv_timeout(b).ok(),
    };
    row.ms = t0.elapsed().as_millis();
    let Some(done) = got else {
        row.verdict = "HANG";
        row.killed = true;
        return (row, true);
    };
    row.killed = cfg.cap_ms.is_some_and(|c| row.ms > u128::from(c));
    match done {
        Done::Translated => row.verdict = "TRANSLATED",
        Done::Panic(msg) => row.error = Some(msg),
        Done::Outcome(Err(Failure::Refused(r))) => {
            row.verdict = "NOTRANS";
            row.refused = Some(r.to_string());
        }
        Done::Outcome(Err(Failure::Cvc5(e))) => row.error = Some(e),
        Done::Outcome(Ok(Outcome::Equivalent)) => row.verdict = "EQ",
        Done::Outcome(Ok(Outcome::Unknown(why))) => {
            row.verdict = "UNKNOWN";
            row.unknown = Some(why);
        }
        Done::Outcome(Ok(Outcome::Different { model, exact })) => {
            row.verdict = "NEQ";
            row.counterexample = Some(model.summary());
            row.model = Some(model);
            row.model_exact = Some(exact);
        }
        Done::Outcome(Ok(Outcome::Incomparable(note))) => {
            row.verdict = "NEQ";
            row.incomparable = Some(note);
        }
    }
    (row, false)
}

/// A case name as a file name.
fn slug(name: &str) -> String {
    let s: String = name.chars().map(|c| if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-') { c } else { '_' }).collect();
    if s.is_empty() { "case".into() } else { s }
}
