// Unless explicitly stated otherwise all files in this repository are licensed under the
// Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/).
// Copyright 2026-Present Datadog, Inc.

//! The Lean axis.
//!
//! sqleq-lean decides one class the frontend refuses outright, `INSERT … VALUES` against
//! `INSERT … SELECT * FROM unnest(…)`, under the gather rule (see docs/LEAN.md). It parses the pair
//! file itself, so it runs over every `.sql` case regardless of what the frontend made of it, and
//! like the second opinion it never moves the exit code outside `--expect pinned`.

use std::path::Path;
use std::time::Instant;

use serde::Serialize;
use serde_json::Value;

use crate::case::Case;
use crate::util::tail;

pub const LEAN_ORDER: [&str; 9] = [
    "proved-gather",
    "no-witness",
    "proved-gather-generated",
    "no-witness-generated",
    "unsupported",
    "invalid-sql",
    "error",
    "timeout",
    "missing",
];
pub const LEAN_PROVED: [&str; 2] = ["proved-gather", "proved-gather-generated"];

/// One record of sqleq-lean's JSON, onto its case.
pub fn attach(x: &mut Case, rec: &Value) {
    let text = |k: &str| rec.get(k).and_then(Value::as_str).unwrap_or("").to_string();
    x.l_verdict = rec.get("verdict").and_then(Value::as_str).map(str::to_string);
    x.l_reason = text("reason");
    x.l_shape = text("shape");
    x.l_ms = rec.get("ms").cloned();
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct Stats {
    pub rows: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub wall_s: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub failed: Option<String>,
}

/// Run sqleq-lean over every `.sql` case and attach its verdicts in place.
pub fn run_lean(cases: &mut [Case], lean_bin: &str, jobs: usize, timeout_s: f64, keep_dir: Option<&Path>) -> Stats {
    let todo: Vec<usize> = (0..cases.len()).filter(|&i| cases[i].is_sql()).collect();
    if todo.is_empty() {
        return Stats::default();
    }
    let Ok(tmp) = crate::util::TempDir::new("sqleq-lean-") else {
        for &i in &todo {
            (cases[i].l_verdict, cases[i].l_reason) = (Some("missing".into()), "no temporary directory".into());
        }
        return Stats { rows: todo.len(), ..Stats::default() };
    };
    let out = tmp.path().join("lean.json");
    let mut argv: Vec<String> = vec![
        lean_bin.into(),
        "--full-names".into(),
        "--json".into(),
        out.to_string_lossy().into_owned(),
        "--jobs".into(),
        jobs.max(1).to_string(),
        "--timeout".into(),
        ((timeout_s * 10.0) as i64).max(60).to_string(),
    ];
    if let Some(k) = keep_dir {
        let k = k.canonicalize().unwrap_or_else(|_| crate::util::abspath(k)).join("lean");
        argv.extend(["--keep".into(), k.to_string_lossy().into_owned()]);
    }
    argv.extend(todo.iter().map(|&i| cases[i].path.clone()));
    let t0 = Instant::now();
    let r = crate::proc::run(&argv, None, &[], None);
    let wall = t0.elapsed().as_secs_f64();
    let got = std::fs::read_to_string(&out).ok().and_then(|t| serde_json::from_str::<Value>(&t).ok());
    let Some(got) = got else {
        let why = tail(&r.err);
        for &i in &todo {
            cases[i].l_verdict = Some("missing".into());
            cases[i].l_reason = if why.is_empty() { "no output".into() } else { why.clone() };
        }
        return Stats { rows: todo.len(), wall_s: Some(wall), failed: Some(why) };
    };
    for &i in &todo {
        match got.get(&cases[i].path) {
            Some(rec) => attach(&mut cases[i], rec),
            None => (cases[i].l_verdict, cases[i].l_reason) = (Some("missing".into()), "no record".into()),
        }
    }
    Stats { rows: todo.len(), wall_s: Some(wall), failed: None }
}
