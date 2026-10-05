// Unless explicitly stated otherwise all files in this repository are licensed under the
// Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/).
// Copyright 2026-Present Datadog, Inc.

//! The sqlsolver axis -- the second opinion.
//!
//! SQLSolver is a second equivalence prover, an independent implementation rather than a variant
//! of the QED prover. sqleq-solver rewrites its proof engine in Rust and reads our lowered `Input`
//! directly; the original is reachable through the bridge in `tools/sqlsolver/`, which hands it
//! the `Input` too. Either way the question asked here is literally the one the QED prover is
//! asked. `docs/SQLSOLVER.md` has the measurements.
//!
//! Three properties of that prover shape the code below, and together they are why this is one
//! batched pass at the end rather than a call inside `run_case`:
//!
//! * the original is a JVM, so per-case startup would swamp the cases themselves;
//! * `Verification.verify` can hang in a way interrupts do not reach, so the driver halts its own
//!   process after writing the offending row and expects the harness to resume on a fresh one --
//!   the loop in [`run_second_opinion`];
//! * its per-row cap is load-sensitive, so the rows go through sequentially even when the prover
//!   pass ran them `-j` wide. A second opinion that changes under load is not one.
//!
//! The vocabulary is deliberately disjoint from the case status, so the two can never be averaged
//! into a single "status". The collapse of `NEQ` and `UNKNOWN` into one bucket is the whole point:
//! that prover does not disprove, so keeping its `NEQ` under its own name would invite someone to
//! read it as a counterexample. `proved-literal` is its tier 0 -- the two plans were already
//! identical -- which is the same fact this harness calls `trivial`, and it is held apart from
//! `proved` for the same reason.

use std::collections::HashMap;
use std::path::Path;

use serde::Serialize;
use serde_json::Value;

use crate::case::{ss_slug, Case};
use crate::discover::SsDriver;
use crate::util::tail;

pub const PROVED: &str = "proved";
pub const PROVED_LITERAL: &str = "proved-literal";
pub const NO_PROOF: &str = "no-proof";
/// Our bridge could not build a plan from the IR.
pub const UNSUPPORTED: &str = "unsupported";
pub const TIMEOUT: &str = "timeout";
pub const ERROR: &str = "error";
/// Never answered: the driver died, or no job was built.
pub const MISSING: &str = "missing";
pub const ORDER: [&str; 7] = [PROVED, PROVED_LITERAL, NO_PROOF, UNSUPPORTED, TIMEOUT, ERROR, MISSING];

/// The drivers' raw verdicts. `NOTRANS` and `NOIR` are ours, not theirs -- the bridge never handed
/// their prover a plan -- and they stay out of `no-proof` for that reason: conflating "we could not
/// express it" with "they could not prove it" is what made their parser look like their prover.
fn bucket_of(verdict: &str) -> &'static str {
    match verdict {
        "EQ" => PROVED,
        "NEQ" | "UNKNOWN" => NO_PROOF,
        "NOTRANS" | "NOIR" => UNSUPPORTED,
        "TIMEOUT" | "HANG" => TIMEOUT,
        _ => ERROR,
    }
}

/// The rows the driver has already written, by name.
///
/// A self-halt can truncate the final line mid-write, so an unparseable tail is dropped rather than
/// trusted: that row is simply re-run on the next pass.
pub fn answered(path: &Path) -> HashMap<String, Value> {
    let mut out = HashMap::new();
    let Ok(text) = std::fs::read_to_string(path) else { return out };
    for line in text.lines().map(str::trim).filter(|l| !l.is_empty()) {
        if let Ok(rec) = serde_json::from_str::<Value>(line) {
            if let Some(name) = rec.get("name").and_then(Value::as_str) {
                out.insert(name.to_string(), rec);
            }
        }
    }
    out
}

/// One answered row, bucketed onto its case.
pub fn attach(case: &mut Case, rec: &Value) {
    case.s_raw = Some(rec.clone());
    case.s_verdict = rec.get("verdict").and_then(Value::as_str).map(str::to_string);
    case.s_ms = rec.get("ms").cloned();
    let mut bucket = bucket_of(case.s_verdict.as_deref().unwrap_or(""));
    let flag = |k: &str| rec.get(k).is_some_and(crate::util::truthy);
    // Their tier 0: the two plans were already identical, so no proving happened. Held apart from a
    // real proof for the same reason this harness holds `trivial` apart from `capability`.
    if bucket == PROVED && flag("literal") {
        bucket = PROVED_LITERAL;
    }
    // Their prover answers UNKNOWN when interrupted, so a row killed at the cap arrives
    // indistinguishable from one it considered and declined. `killed` is the only thing that
    // separates them, and the distinction is the whole point of a `timeout` bucket: "we stopped
    // asking" is not "they found no proof". An EQ is exempt -- a proof that landed on the boundary
    // is still a claim.
    if flag("killed") && bucket != PROVED && bucket != PROVED_LITERAL {
        bucket = TIMEOUT;
    }
    case.s_bucket = Some(bucket.to_string());
    let note = ["refused", "error"]
        .iter()
        .filter_map(|k| rec.get(*k).and_then(Value::as_str))
        .find(|s| !s.is_empty())
        .unwrap_or("");
    case.s_note = tail(note);
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct Stalled {
    pub exit: i32,
    pub reason: String,
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct Stats {
    pub rows: usize,
    pub passes: usize,
    pub halts: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub answered: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stalled: Option<Stalled>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub wall_s: Option<f64>,
}

/// Ask the second prover about every case that produced a job, and attach the answers.
///
/// Mutates the cases in place, because the answer belongs beside the case rather than in a
/// parallel table that a later sort could desynchronise.
///
/// `slots` drivers run side by side, each over its own share of the jobs and its own results file.
/// One is the default and the reproducible choice: the per-row cap is load-sensitive, so a row near
/// it can decide differently with other drivers running. `mem_bytes` caps each driver's address
/// space.
pub fn run_second_opinion(
    cases: &mut [Case],
    ss_dir: &Path,
    driver: &SsDriver,
    timeout_ms: u64,
    slots: usize,
    mem_bytes: Option<u64>,
) -> Stats {
    let mut jobs: Vec<(String, String)> = Vec::new(); // (name, the job's line)
    for case in cases.iter_mut() {
        let job = ss_dir.join(format!("{}.job.jsonl", ss_slug(&case.name)));
        if !job.exists() {
            // Refused, or lowered but not packageable. Either way the bridge never got a plan, and
            // that is ours, not theirs -- hence `unsupported`.
            if case.s_bucket.is_none() {
                case.s_bucket = Some(UNSUPPORTED.into());
                // Prefer the frontend's own reason: on a refused row it names the construct, which
                // is what a reader of this column wants.
                if case.s_note.is_empty() {
                    let why = tail(&case.message);
                    case.s_note = if why.is_empty() { "no plan to bridge".into() } else { why };
                }
            }
            continue;
        }
        let line = std::fs::read_to_string(&job).map_err(|e| e.to_string()).and_then(|t| {
            let first = t.lines().next().ok_or("empty job file")?.to_string();
            let v: Value = serde_json::from_str(&first).map_err(|e| e.to_string())?;
            let name = v.get("name").and_then(Value::as_str).ok_or("job has no name")?.to_string();
            Ok((name, first))
        });
        match line {
            Ok(j) => jobs.push(j),
            Err(e) => {
                case.s_bucket = Some(ERROR.into());
                case.s_note = format!("unreadable job: {e}");
            }
        }
    }
    if jobs.is_empty() {
        return Stats::default();
    }

    let slots = slots.max(1).min(jobs.len());
    // One slot keeps the file names a `--keep` user has always found here.
    let file = |stem: &str, k: usize| {
        if slots == 1 {
            ss_dir.join(format!("{stem}.jsonl"))
        } else {
            ss_dir.join(format!("{stem}.{k}.jsonl"))
        }
    };
    let shares: Vec<Vec<&(String, String)>> =
        (0..slots).map(|k| jobs.iter().skip(k).step_by(slots).collect()).collect();
    let runs: Vec<Slot> = std::thread::scope(|s| {
        let handles: Vec<_> = shares
            .iter()
            .enumerate()
            .map(|(k, share)| {
                let (todo, out) = (file("todo", k), file("results", k));
                s.spawn(move || run_slot(share, &todo, &out, driver, timeout_ms, mem_bytes))
            })
            .collect();
        handles.into_iter().map(|h| h.join().unwrap_or_default()).collect()
    });
    let passes = runs.iter().map(|r| r.passes).sum();
    let halts = runs.iter().map(|r| r.halts).sum();
    let stall = runs.into_iter().find_map(|r| r.stall);

    let mut got = HashMap::new();
    for k in 0..slots {
        got.extend(answered(&file("results", k)));
    }
    for case in cases.iter_mut() {
        match got.get(&case.name) {
            Some(rec) => attach(case, rec),
            None => {
                if case.s_bucket.is_none() {
                    case.s_bucket = Some(MISSING.into());
                    let why = stall.as_ref().map(|s| s.reason.clone()).unwrap_or_default();
                    case.s_note = if why.is_empty() { "no answer".into() } else { why };
                }
            }
        }
    }
    Stats { rows: jobs.len(), passes, halts, answered: Some(got.len()), stalled: stall, wall_s: None }
}

#[derive(Default)]
struct Slot {
    passes: usize,
    halts: usize,
    stall: Option<Stalled>,
}

/// One driver's share of the jobs, run to the end: resumed after every self-halt, and past a row
/// the driver died on.
fn run_slot(
    jobs: &[&(String, String)],
    todo_path: &Path,
    out_path: &Path,
    driver: &SsDriver,
    timeout_ms: u64,
    mem_bytes: Option<u64>,
) -> Slot {
    let mut slot = Slot::default();
    loop {
        let have = answered(out_path);
        let todo: Vec<&&(String, String)> = jobs.iter().filter(|(n, _)| !have.contains_key(n)).collect();
        let Some(first) = todo.first() else { break };
        let body: String = todo.iter().map(|(_, l)| format!("{l}\n")).collect();
        if let Err(e) = std::fs::write(todo_path, body) {
            slot.stall = Some(Stalled { exit: -1, reason: format!("{}: {e}", todo_path.display()) });
            break;
        }
        slot.passes += 1;
        let mut argv = driver.cmd.clone();
        argv.push(todo_path.to_string_lossy().into_owned());
        argv.push(out_path.to_string_lossy().into_owned());
        argv.push(format!("--timeout-ms={timeout_ms}"));
        let r = crate::proc::run_limited(&argv, Some(&driver.cwd), &driver.env, None, mem_bytes);
        // Exit 3 is the driver taking its own process down because a row would not stop (the JVM's
        // interrupt missed; sqleq-solver's grace period ran out). It writes the row first, so
        // resuming always advances.
        if r.rc == 3 {
            slot.halts += 1;
            continue;
        }
        if answered(out_path).len() > have.len() {
            continue;
        }
        if r.rc < 0 {
            // Killed by a signal before answering the row it was on: out of memory under a cap, or
            // a crash. That is this row's answer, not a reason to give up on the rest -- record it
            // and resume past it. The row is the first unanswered one, which is the one it was on.
            let died = serde_json::json!({
                "name": first.0, "verdict": "ERROR", "killed": false, "died": true,
                "error": format!("sqleq-solver died on this row (signal {})", -r.rc),
            });
            let append = std::fs::OpenOptions::new()
                .append(true)
                .create(true)
                .open(out_path)
                .and_then(|mut f| std::io::Write::write_all(&mut f, format!("{died}\n").as_bytes()));
            if append.is_ok() {
                continue;
            }
        }
        // No row was answered and the process exited on its own: a classpath or native library
        // problem, not a hard case. Report it rather than spinning.
        slot.stall = Some(Stalled { exit: r.rc, reason: tail(&r.err) });
        break;
    }
    slot
}

/// How the output names the second prover, from its driver's `imp`.
pub fn name(imp: &str) -> &'static str {
    if imp == "sqleq-solver" {
        "sqleq-solver"
    } else {
        "the JVM SQLSolver"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn rows_bucket_like_the_jvm_drivers() {
        let rows = [
            (json!({"verdict": "EQ", "literal": true}), PROVED_LITERAL),
            (json!({"verdict": "EQ", "killed": true}), PROVED),
            (json!({"verdict": "NEQ"}), NO_PROOF),
            (json!({"verdict": "UNKNOWN", "killed": true}), TIMEOUT),
            (json!({"verdict": "NOIR", "refused": "line one\nunknown table t"}), UNSUPPORTED),
            (json!({"verdict": "HANG", "killed": true}), TIMEOUT),
            (json!({"verdict": "WHAT"}), ERROR),
        ];
        for (rec, want) in rows {
            let mut c = Case::new("x", "x");
            attach(&mut c, &rec);
            assert_eq!(c.s_bucket.as_deref(), Some(want), "{rec}");
        }
        let mut c = Case::new("x", "x");
        attach(&mut c, &json!({"verdict": "NOIR", "refused": "line one\nunknown table t"}));
        assert_eq!(c.s_note, "unknown table t");
    }
}
