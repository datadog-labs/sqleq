// Unless explicitly stated otherwise all files in this repository are licensed under the
// Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/).
// Copyright 2026-Present Datadog, Inc.

//! The fuzz axis.
//!
//! sqleq-fuzz is the only axis that can refute: it runs both statements on random instances in
//! PostgreSQL and compares the results. It reads the pair file itself and binds `$N` on its own, so it
//! needs no frontend and ignores the catalog header. The trial budget is always passed explicitly
//! -- the tool's defaults are free to change, and a pinned `no-counterexample` is only a claim about
//! one budget.

use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;
use std::time::Instant;

use serde::Serialize;
use serde_json::{json, Value};

use crate::case::Case;
use crate::inputs::CorpusRow;
use crate::util::{abspath, tail};

pub const FUZZ_ARGS: [&str; 6] = ["--trials", "120", "--rows", "5", "--seed", "0"];

/// The label's kind is the part before the first `:` (`ERROR:…`, `PARAM-MISALIGNED:…`).
fn word_of(kind: &str) -> &'static str {
    match kind {
        "NOT-EQUIVALENT" => "counterexample",
        "NO-COUNTEREXAMPLE" => "no-counterexample",
        "PARAM-MISALIGNED" => "param-misaligned",
        "NOT-COMPARABLE" => "not-comparable",
        "NONDET-SKIP" => "nondet-skip",
        "NO-SCHEMA" => "no-schema",
        "NO-TABLES" => "no-tables",
        _ => "error",
    }
}

/// What sqleq-fuzz tests: a pair file, or a corpus row.
pub enum Pair<'a> {
    File(&'a str),
    Row(&'a CorpusRow),
}

impl<'a> Pair<'a> {
    pub fn of(case: &'a Case) -> Option<Pair<'a>> {
        match &case.row {
            Some(r) => Some(Pair::Row(r)),
            None if case.is_sql() => Some(Pair::File(&case.path)),
            None => None,
        }
    }
}

/// One answer: the label's kind, the counterexample or the reason, the wall time, and sqleq-fuzz's
/// own record -- the full label, and how many trials really compared both sides when not all did.
pub struct Answer {
    pub word: String,
    pub note: String,
    pub ms: u64,
    pub raw: Option<Value>,
}

impl Answer {
    pub fn attach(self, c: &mut Case) {
        (c.f_verdict, c.f_note, c.f_ms, c.f_raw) = (Some(self.word), self.note, Some(self.ms), self.raw);
    }
}

/// sqleq-fuzz on one pair. A corpus row goes through its `row` mode, handed the row as a one-row
/// CSV: that is the code its `csv` mode runs on every row, raw DDL and all, so a row is fuzzed here
/// exactly as it is in a whole-corpus run.
pub fn fuzz_one(fuzz_bin: &str, pair: Pair, timeout_s: f64) -> Answer {
    let tmp;
    let (argv_tail, dir): (Vec<String>, std::path::PathBuf) = match pair {
        Pair::File(path) => {
            let path = abspath(Path::new(path));
            let dir = path.parent().unwrap_or(Path::new("/")).to_path_buf();
            (vec!["file".into(), path.to_string_lossy().into_owned()], dir)
        }
        Pair::Row(row) => {
            tmp = match crate::util::TempDir::new("sqleq-fuzz-row-") {
                Ok(t) => t,
                Err(e) => return Answer { word: "error".into(), note: format!("no temporary directory: {e}"), ms: 0, raw: None },
            };
            if let Err(e) = row.write_csv(tmp.path()) {
                return Answer { word: "error".into(), note: format!("cannot write the row: {e}"), ms: 0, raw: None };
            }
            (vec!["row".into(), "row.csv".into(), "0".into()], tmp.path().to_path_buf())
        }
    };
    let mut argv = vec![fuzz_bin.to_string()];
    argv.extend(argv_tail);
    argv.extend(FUZZ_ARGS.iter().map(|s| s.to_string()));
    let r = crate::proc::run_cmd(&argv, &dir, timeout_s, &[]);
    let ms = (r.wall * 1000.0) as u64;
    if r.timed_out {
        return Answer { word: "timeout".into(), note: String::new(), ms, raw: None };
    }
    let lines: Vec<&str> = r.out.lines().map(str::trim).filter(|l| !l.is_empty()).collect();
    if r.rc != 0 || lines.is_empty() {
        let why = tail(&r.err);
        let note = if why.is_empty() { format!("sqleq-fuzz exit {}", r.rc) } else { why };
        return Answer { word: "error".into(), note, ms, raw: None };
    }
    let label = lines[0];
    let word = word_of(label.split(':').next().unwrap_or(""));
    let mut note = lines[1..]
        .iter()
        .find_map(|l| l.strip_prefix("counterexample: "))
        .unwrap_or("")
        .to_string();
    if note.is_empty() {
        if let Some((_, rest)) = label.split_once(':') {
            note = rest.trim().to_string();
        }
    }
    let mut raw = json!({ "label": label });
    let partial = lines[1..].iter().find_map(|l| l.strip_prefix("partial: "));
    if let Some((ok, err)) = partial.and_then(|p| p.split_once(" trials compared both sides; last error: ")) {
        raw["ok_trials"] = ok.parse::<u64>().map_or(json!(ok), |n| json!(n));
        raw["trial_error"] = json!(err);
    }
    Answer { word: word.into(), note, ms, raw: Some(raw) }
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct Stats {
    pub rows: usize,
    /// The pass's own wall time; a portfolio has no separate pass to time.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub wall_s: Option<f64>,
}

/// Run sqleq-fuzz over every case with a pair -- `.sql` files and corpus rows -- and attach its
/// verdicts in place, calling `done` on each case as its answer lands.
pub fn run_fuzz(cases: &mut [Case], fuzz_bin: &str, jobs: usize, timeout_s: f64, done: &(dyn Fn(&Case) + Sync)) -> Stats {
    let t0 = Instant::now();
    let next = AtomicUsize::new(0);
    let shared: Vec<Mutex<&mut Case>> = cases.iter_mut().filter(|c| c.has_pair()).map(Mutex::new).collect();
    std::thread::scope(|s| {
        for _ in 0..jobs.max(1).min(shared.len().max(1)) {
            s.spawn(|| loop {
                let k = next.fetch_add(1, Ordering::Relaxed);
                let Some(slot) = shared.get(k) else { break };
                let mut c = slot.lock().unwrap_or_else(|e| e.into_inner());
                if let Some(pair) = Pair::of(&c) {
                    let a = fuzz_one(fuzz_bin, pair, timeout_s);
                    a.attach(&mut c);
                    done(&c);
                }
            });
        }
    });
    Stats { rows: shared.len(), wall_s: Some(crate::util::round(t0.elapsed().as_secs_f64(), 3)) }
}
