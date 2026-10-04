// Unless explicitly stated otherwise all files in this repository are licensed under the
// Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/).
// Copyright 2026-Present Datadog, Inc.

//! The fuzz axis.
//!
//! sqleq-fuzz is the only axis that can refute: it runs both statements on random instances in
//! DuckDB and compares the results. It reads the pair file itself and binds `$N` on its own, so it
//! needs no frontend and ignores the catalog header. The trial budget is always passed explicitly
//! -- the tool's defaults are free to change, and a pinned `no-counterexample` is only a claim about
//! one budget.

use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;
use std::time::Instant;

use serde::Serialize;

use crate::case::Case;
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

/// (word, note, ms) for one pair file.
pub fn fuzz_one(fuzz_bin: &str, path: &str, timeout_s: f64) -> (String, String, u64) {
    let path = abspath(Path::new(path));
    let dir = path.parent().unwrap_or(Path::new("/")).to_path_buf();
    let mut argv = vec![fuzz_bin.to_string(), "file".into(), path.to_string_lossy().into_owned()];
    argv.extend(FUZZ_ARGS.iter().map(|s| s.to_string()));
    let r = crate::proc::run_cmd(&argv, &dir, timeout_s, &[]);
    let ms = (r.wall * 1000.0) as u64;
    if r.timed_out {
        return ("timeout".into(), String::new(), ms);
    }
    let lines: Vec<&str> = r.out.lines().map(str::trim).filter(|l| !l.is_empty()).collect();
    if r.rc != 0 || lines.is_empty() {
        let why = tail(&r.err);
        return ("error".into(), if why.is_empty() { format!("sqleq-fuzz exit {}", r.rc) } else { why }, ms);
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
    (word.into(), note, ms)
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct Stats {
    pub rows: usize,
    /// The pass's own wall time; a portfolio has no separate pass to time.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub wall_s: Option<f64>,
}

/// Run sqleq-fuzz over every `.sql` case and attach its verdicts in place.
pub fn run_fuzz(cases: &mut [Case], fuzz_bin: &str, jobs: usize, timeout_s: f64) -> Stats {
    let t0 = Instant::now();
    let todo: Vec<usize> = (0..cases.len()).filter(|&i| cases[i].is_sql()).collect();
    let paths: Vec<String> = todo.iter().map(|&i| cases[i].path.clone()).collect();
    let results: Mutex<Vec<Option<(String, String, u64)>>> = Mutex::new(vec![None; todo.len()]);
    let next = AtomicUsize::new(0);
    std::thread::scope(|s| {
        for _ in 0..jobs.max(1).min(paths.len().max(1)) {
            s.spawn(|| loop {
                let k = next.fetch_add(1, Ordering::Relaxed);
                let Some(p) = paths.get(k) else { break };
                let r = fuzz_one(fuzz_bin, p, timeout_s);
                results.lock().unwrap_or_else(|e| e.into_inner())[k] = Some(r);
            });
        }
    });
    let results = results.into_inner().unwrap_or_else(|e| e.into_inner());
    for (k, r) in results.into_iter().enumerate() {
        if let Some((w, n, ms)) = r {
            let c = &mut cases[todo[k]];
            (c.f_verdict, c.f_note, c.f_ms) = (Some(w), n, Some(ms));
        }
    }
    Stats { rows: todo.len(), wall_s: Some(crate::util::round(t0.elapsed().as_secs_f64(), 3)) }
}
