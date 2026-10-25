// Unless explicitly stated otherwise all files in this repository are licensed under the
// Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/).
// Copyright 2026-Present Datadog, Inc.

//! The cvc5 axis.
//!
//! `sqleq-cvc5` encodes the lowered `Input` -- the same plan the QED prover and sqleq-solver read
//! -- as an SMT problem over cvc5's theory of bags (or sets) of tuples, after the LPAR'24 relations
//! work's Java translator, and asks cvc5 for a model. `unsat` is a proof under that encoding; `sat`
//! comes with the model's tables, a database on which the two encodings differ. `docs/CVC5.md`
//! says what the encoding reads literally and what it approximates.
//!
//! Unlike the SQLSolver axes, this one can refute, and unlike the fuzz axis its refutation is not
//! an executed counterexample: it is a database under the encoding's readings of the types, which
//! cvc5 checked against its own assertions (`check-models`), and under the plan's constraints
//! alone -- the frontend drops those it cannot carry, so the database may be one the DDL forbids.
//! The bucket is therefore `model`, a word of its own, so that a reader never confuses it with
//! sqleq-fuzz's `counterexample`; the suite reads it as a refutation under the plan's own binding
//! (`suite::IR`), where every prover's proof also claims: a model and a proof over one plan are an
//! alarm, and a model alone decides no verdict.
//!
//! The driver is cheap to start and stops on its own at cvc5's time limit, so the axis runs one
//! process per case, in parallel like sqleq-fuzz, with the case's deadline as the backstop: cvc5
//! does not always notice its limit inside a long rewrite, and a process can be killed where a
//! thread cannot. The job is the plan itself under the case's name (`write_job`), packaged while
//! the case's working directory still exists.

use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;
use std::time::Instant;

use serde::Serialize;
use serde_json::Value;

use crate::case::{ss_slug, Case};
use crate::discover::Cvc5Driver;
use crate::util::tail;

pub const PROVED: &str = "proved";
pub const PROVED_LITERAL: &str = "proved-literal";
/// cvc5 found a database on which its encodings of the two queries differ.
pub const MODEL: &str = "model";
/// `unknown`, or a pair whose two queries return rows of different sorts, which the encoding
/// cannot compare.
pub const NO_PROOF: &str = "no-proof";
/// The plan was not encoded: a construct with no faithful term, or no plan at all.
pub const UNSUPPORTED: &str = "unsupported";
pub const TIMEOUT: &str = "timeout";
pub const ERROR: &str = "error";
/// Never answered.
pub const MISSING: &str = "missing";
pub const ORDER: [&str; 8] = [PROVED, PROVED_LITERAL, MODEL, NO_PROOF, UNSUPPORTED, TIMEOUT, ERROR, MISSING];

/// The driver's raw verdicts, bucketed. `NEQ` is a `model` only with an exact model: one found
/// with no uninterpreted symbol in the problem, so that it is a database and nothing more. A model
/// under an uninterpreted function also fixes that function's values, which the real one may not
/// take, and so is a candidate rather than a claim; and an `NEQ` with no model says the two
/// queries' rows have different sorts, which the IR's guard types can make of two queries that
/// agree on every database. Both are `no-proof`, with the reason in the note.
fn bucket_of(rec: &Value) -> &'static str {
    let exact = rec.get("model_exact").is_none_or(crate::util::truthy);
    match rec.get("verdict").and_then(Value::as_str).unwrap_or("") {
        "EQ" => PROVED,
        "NEQ" if rec.get("model").is_some() && exact => MODEL,
        "NEQ" | "UNKNOWN" => NO_PROOF,
        "NOTRANS" | "NOIR" => UNSUPPORTED,
        "TIMEOUT" | "HANG" => TIMEOUT,
        _ => ERROR,
    }
}

/// The rows a driver wrote, by name; a torn last line is dropped.
pub fn answered(path: &Path) -> std::collections::HashMap<String, Value> {
    let mut out = std::collections::HashMap::new();
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
    case.c_raw = Some(rec.clone());
    case.c_verdict = rec.get("verdict").and_then(Value::as_str).map(str::to_string);
    case.c_ms = rec.get("ms").cloned();
    let mut bucket = bucket_of(rec);
    let flag = |k: &str| rec.get(k).is_some_and(crate::util::truthy);
    if bucket == PROVED && flag("literal") {
        bucket = PROVED_LITERAL;
    }
    // A row cut off at the cap arrives as `UNKNOWN`; `killed` tells it from one cvc5 gave up on.
    // A proof or a model that landed on the boundary is still a claim.
    if flag("killed") && bucket != PROVED && bucket != PROVED_LITERAL && bucket != MODEL {
        bucket = TIMEOUT;
    }
    case.c_bucket = Some(bucket.to_string());
    let text = |k: &str| rec.get(k).and_then(Value::as_str).filter(|s| !s.is_empty()).map(str::to_string);
    case.c_note = match bucket {
        MODEL => text("counterexample").unwrap_or_default(),
        NO_PROOF if rec.get("model").is_some() => {
            format!("candidate model, under uninterpreted symbols: {}", text("counterexample").unwrap_or_default())
        }
        NO_PROOF => text("incomparable").or_else(|| text("unknown")).unwrap_or_default(),
        UNSUPPORTED => text("refused").unwrap_or_default(),
        _ => text("error").or_else(|| text("refused")).map(|s| tail(&s)).unwrap_or_default(),
    };
}

/// The driver's job for a case: the lowered plan under the case's name, one line. The plan's text
/// goes in as it is -- it is JSON already, and real plans nest past serde_json's default depth --
/// with its line breaks taken out, so the job stays one line.
pub fn write_job(name: &str, plan: &Path, job: &Path) -> std::io::Result<()> {
    let text = std::fs::read_to_string(plan)?;
    let ir: String = text.lines().map(str::trim).collect();
    if ir.is_empty() {
        return Err(std::io::Error::other("the plan is empty"));
    }
    let name = serde_json::to_string(name).map_err(std::io::Error::other)?;
    std::fs::write(job, format!("{{\"name\":{name},\"ir\":{ir}}}\n"))
}

pub enum Answer {
    Row(Value),
    CutOff,
    Failed(String),
}

/// sqleq-cvc5 on one job, within `timeout_s`: cvc5's own limit is set a margin short of it, so the
/// driver can write its row; the process-group kill at the deadline is the backstop.
pub fn cvc5_one(driver: &Cvc5Driver, name: &str, job: &Path, out: &Path, cap_ms: Option<u64>, timeout_s: f64) -> Answer {
    let _ = std::fs::remove_file(out);
    let remaining_ms = if timeout_s.is_finite() { (timeout_s * 1000.0) as u64 } else { u64::MAX };
    if remaining_ms == 0 {
        return Answer::CutOff;
    }
    let margin = (remaining_ms / 10).clamp(50, 2000);
    let cap = remaining_ms.saturating_sub(2 * margin).max(1).min(cap_ms.unwrap_or(u64::MAX));
    let mut argv = driver.cmd();
    argv.extend([
        job.to_string_lossy().into_owned(),
        out.to_string_lossy().into_owned(),
        format!("--timeout-ms={cap}"),
        format!("--grace-ms={margin}"),
    ]);
    let cwd = job.parent().unwrap_or(Path::new("."));
    let r = crate::proc::run(&argv, Some(cwd), &[], timeout_s.is_finite().then_some(timeout_s));
    if let Some(row) = answered(out).remove(name) {
        return Answer::Row(row);
    }
    if r.timed_out {
        return Answer::CutOff;
    }
    let why = tail(&r.err);
    Answer::Failed(if why.is_empty() { format!("sqleq-cvc5 exit {}", r.rc) } else { why })
}

/// An answer onto its case.
pub fn settle(case: &mut Case, answer: Answer) {
    match answer {
        Answer::Row(row) => attach(case, &row),
        Answer::CutOff => {
            case.c_bucket = Some(TIMEOUT.into());
            case.c_note = "still running at the deadline".into();
        }
        Answer::Failed(why) => {
            case.c_bucket = Some(ERROR.into());
            case.c_note = why;
        }
    }
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct Stats {
    pub rows: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub wall_s: Option<f64>,
}

/// Ask cvc5 about every case whose plan was packaged in `dir`, `jobs` at a time, each within
/// `timeout_s`, and attach the answers in place, calling `done` on each case as its answer lands.
/// A case with no job -- refused, or not lowered -- is `unsupported`, with the frontend's reason.
pub fn run_cvc5(
    cases: &mut [Case],
    dir: &Path,
    driver: &Cvc5Driver,
    jobs: usize,
    cap_ms: Option<u64>,
    timeout_s: f64,
    done: &(dyn Fn(&Case) + Sync),
) -> Stats {
    let t0 = Instant::now();
    for case in cases.iter_mut() {
        let job = dir.join(format!("{}.job.jsonl", ss_slug(&case.name)));
        if !job.exists() && case.c_bucket.is_none() {
            case.c_bucket = Some(UNSUPPORTED.into());
            let why = tail(&case.message);
            case.c_note = if why.is_empty() { "no plan to encode".into() } else { why };
        }
    }
    let todo: Vec<Mutex<&mut Case>> = cases.iter_mut().filter(|c| c.c_bucket.is_none()).map(Mutex::new).collect();
    let next = AtomicUsize::new(0);
    std::thread::scope(|s| {
        for _ in 0..jobs.max(1).min(todo.len().max(1)) {
            s.spawn(|| loop {
                let k = next.fetch_add(1, Ordering::Relaxed);
                let Some(slot) = todo.get(k) else { break };
                let mut c = slot.lock().unwrap_or_else(|e| e.into_inner());
                let slug = ss_slug(&c.name);
                let job = dir.join(format!("{slug}.job.jsonl"));
                let out = dir.join(format!("{slug}.results.jsonl"));
                let a = cvc5_one(driver, &c.name, &job, &out, cap_ms, timeout_s);
                settle(&mut c, a);
                done(&c);
            });
        }
    });
    Stats { rows: todo.len(), wall_s: Some(crate::util::round(t0.elapsed().as_secs_f64(), 3)) }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn rows_bucket_and_only_a_model_refutes() {
        let rows = [
            (json!({"verdict": "EQ"}), PROVED, ""),
            (json!({"verdict": "EQ", "literal": true}), PROVED_LITERAL, ""),
            (json!({"verdict": "NEQ", "model": {"tables": []}, "counterexample": "t=[(1)]"}), MODEL, "t=[(1)]"),
            (json!({"verdict": "NEQ", "model": {"tables": []}, "model_exact": true, "counterexample": "t=[(1)]"}), MODEL, "t=[(1)]"),
            (
                json!({"verdict": "NEQ", "model": {"tables": []}, "model_exact": false, "counterexample": "t=[(1)]"}),
                NO_PROOF,
                "candidate model, under uninterpreted symbols: t=[(1)]",
            ),
            (json!({"verdict": "NEQ", "incomparable": "the two queries return 1 and 2 columns"}), NO_PROOF, "the two queries return 1 and 2 columns"),
            (json!({"verdict": "UNKNOWN", "unknown": "TIMEOUT"}), NO_PROOF, "TIMEOUT"),
            (json!({"verdict": "UNKNOWN", "killed": true}), TIMEOUT, ""),
            (json!({"verdict": "NEQ", "model": {"tables": []}, "killed": true}), MODEL, ""),
            (json!({"verdict": "NOTRANS", "refused": "correlated"}), UNSUPPORTED, "correlated"),
            (json!({"verdict": "HANG", "killed": true}), TIMEOUT, ""),
            (json!({"verdict": "ERROR", "error": "a\nb"}), ERROR, "b"),
        ];
        for (rec, want, note) in rows {
            let mut c = Case::new("x", "x");
            attach(&mut c, &rec);
            assert_eq!(c.c_bucket.as_deref(), Some(want), "{rec}");
            assert_eq!(c.c_note, note, "{rec}");
        }
    }

    #[test]
    fn the_job_is_the_plan_under_the_name() {
        let dir = crate::util::TempDir::new("sqleq-cvc5-job-").unwrap();
        let plan = dir.path().join("p.json");
        std::fs::write(&plan, r#"{"schemas": [], "queries": [{"scan": 0}, {"scan": 0}]}"#).unwrap();
        let job = dir.path().join("p.job.jsonl");
        write_job("a/b.sql", &plan, &job).unwrap();
        let line: Value = serde_json::from_str(std::fs::read_to_string(&job).unwrap().trim()).unwrap();
        assert_eq!(line["name"], "a/b.sql");
        assert_eq!(line["ir"]["queries"][0], json!({"scan": 0}));
    }
}
