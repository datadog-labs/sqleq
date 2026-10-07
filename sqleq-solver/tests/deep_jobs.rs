// Unless explicitly stated otherwise all files in this repository are licensed under the
// Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/).
// Copyright 2026-Present Datadog, Inc.

//! The binary answers a pathologically deep plan with a refusal row and goes on to the next job,
//! instead of overflowing a stack, which aborts the whole run.

use std::path::PathBuf;
use std::process::Command;

use serde_json::Value;

// The plans are written as text: a `serde_json::Value` this deep would overflow the test's own
// stack when dropped.

const EQ_ONE: &str = r#"{"operator":"=","type":"BOOLEAN","operand":[{"column":0,"type":"INTEGER"},{"operator":"1","operand":[],"type":"INTEGER"}]}"#;

/// `NOT NOT ... (id = 1)`, `depth` times.
fn not_chain(depth: usize) -> String {
    let open = r#"{"operator":"NOT","type":"BOOLEAN","operand":["#;
    format!("{}{EQ_ONE}{}", open.repeat(depth), "]}".repeat(depth))
}

/// `id + 1 + ... + 1 = 5`, with `terms` ones.
fn plus_chain(terms: usize) -> String {
    let open = r#"{"operator":"+","type":"INTEGER","operand":["#;
    let close = r#",{"operator":"1","operand":[],"type":"INTEGER"}]}"#;
    let sum = format!(r#"{}{{"column":0,"type":"INTEGER"}}{}"#, open.repeat(terms), close.repeat(terms));
    format!(r#"{{"operator":"=","type":"BOOLEAN","operand":[{sum},{{"operator":"5","operand":[],"type":"INTEGER"}}]}}"#)
}

/// A job comparing `SELECT id FROM t WHERE <a>` with `... WHERE <b>`.
fn job(name: &str, a: &str, b: &str) -> String {
    let query = |cond: &str| {
        format!(r#"{{"project":{{"source":{{"filter":{{"source":{{"scan":0}},"condition":{cond}}}}},"target":[{{"column":0,"type":"INTEGER"}}]}}}}"#)
    };
    let schemas = r#"[{"name":"t","types":["INTEGER"],"key":[],"nullable":[true]}]"#;
    format!(r#"{{"name":"{name}","schema":"","ir":{{"schemas":{schemas},"queries":[{},{}]}}}}"#, query(a), query(b))
}

fn run(name: &str, jobs: &[String]) -> (Option<i32>, Vec<Value>) {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!("deep-jobs-{name}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let (input, output) = (dir.join("jobs.jsonl"), dir.join("results.jsonl"));
    std::fs::write(&input, jobs.join("\n") + "\n").unwrap();
    let _ = std::fs::remove_file(&output);
    let status = Command::new(env!("CARGO_BIN_EXE_sqleq-solver")).arg(&input).arg(&output).output().unwrap().status;
    let rows = std::fs::read_to_string(&output)
        .unwrap_or_default()
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    std::fs::remove_dir_all(&dir).unwrap();
    (status.code(), rows)
}

#[test]
fn a_plan_past_the_depth_bound_is_refused_and_the_run_goes_on() {
    let jobs = [
        job("deep-not", &not_chain(5_000), EQ_ONE),
        job("deep-plus", &plus_chain(3_000), &plus_chain(2_999)),
        job("after", EQ_ONE, &not_chain(2)),
    ];
    let (code, rows) = run("past", &jobs);
    assert_eq!(code, Some(0), "rows: {rows:?}");
    let verdicts: Vec<(&str, &str, Option<&str>)> = rows
        .iter()
        .map(|r| (r["name"].as_str().unwrap(), r["verdict"].as_str().unwrap(), r["refused"].as_str()))
        .collect();
    assert_eq!(
        verdicts,
        [
            ("deep-not", "NOTRANS", Some("nesting-too-deep")),
            ("deep-plus", "NOTRANS", Some("nesting-too-deep")),
            ("after", "EQ", None),
        ]
    );
}

#[test]
fn a_plan_just_inside_the_depth_bound_is_answered() {
    // About 2 JSON levels per term, under `ir::MAX_DEPTH`, and far past what a default 2 MiB
    // thread stack takes through every stage.
    let jobs = [job("not", &not_chain(990), &not_chain(988)), job("plus", &plus_chain(990), &plus_chain(989))];
    let (code, rows) = run("inside", &jobs);
    assert_eq!(code, Some(0), "rows: {rows:?}");
    assert_eq!(rows.len(), 2, "rows: {rows:?}");
    for r in &rows {
        assert_ne!(r["verdict"], "NOTRANS", "{r}");
        assert_ne!(r["verdict"], "ERROR", "{r}");
    }
}
