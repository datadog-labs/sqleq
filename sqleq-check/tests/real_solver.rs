// Unless explicitly stated otherwise all files in this repository are licensed under the
// Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/).
// Copyright 2026-Present Datadog, Inc.

//! The second opinion end to end against the real sqleq-solver: the driver reads the jobs, writes
//! IrDriver-shaped rows, and the harness buckets them exactly as it buckets the JVM's.
//!
//! sqleq-solver needs Z3 to build, so this skips when no build of it is found -- unless
//! `$SQLEQ_SOLVER_BIN` names one, when a missing binary is a failure. CI's solver job sets it.

use serde_json::{json, Value};
use sqleq_check::axes::solver::{self, run_second_opinion};
use sqleq_check::portfolio;
use sqleq_check::case::{ss_slug, Case};
use sqleq_check::discover::{repo, SsDriver};
use sqleq_check::util::{is_exe, TempDir};

fn sqleq_solver() -> Option<SsDriver> {
    let bin = match std::env::var("SQLEQ_SOLVER_BIN").ok().filter(|b| !b.is_empty()) {
        // Relative to the repository root, where CI names it from: `cargo test` runs this from the
        // crate's own directory.
        Some(b) => {
            let p = repo().join(&b);
            assert!(is_exe(&p), "$SQLEQ_SOLVER_BIN={b} is not an executable");
            p
        }
        None => ["release", "debug"].iter().map(|p| repo().join("target").join(p).join("sqleq-solver")).find(|b| is_exe(b))?,
    };
    let bin = bin.canonicalize().unwrap().to_string_lossy().into_owned();
    Some(SsDriver { imp: "sqleq-solver".into(), cmd: vec![bin.clone()], cwd: repo().to_path_buf(), env: Vec::new(), location: bin })
}

fn job(dir: &std::path::Path, name: &str, ir: Value, refusal: Option<&str>) -> Case {
    let mut row = json!({"name": name, "ir": ir, "schema": ""});
    if let Some(r) = refusal {
        row["refusal"] = json!(r);
    }
    std::fs::write(dir.join(format!("{}.job.jsonl", ss_slug(name))), format!("{row}\n")).unwrap();
    Case::new(name, name)
}

#[test]
fn rows_are_bucketed_like_the_jvm_drivers() {
    let Some(driver) = sqleq_solver() else {
        eprintln!("skipped: sqleq-solver is not built (set $SQLEQ_SOLVER_BIN to require it)");
        return;
    };
    let dir = TempDir::new("sqleq-ss-test-").unwrap();
    let d = dir.path();
    let schema = json!([{"types": ["INTEGER"], "key": [], "nullable": [true]}]);
    let scan = json!({"scan": 0});
    let filtered = json!({"filter": {"source": scan, "condition": {"operator": "=", "type": "BOOLEAN", "operand": [
        {"column": 0, "type": "INTEGER"}, {"operator": "1", "operand": [], "type": "INTEGER"}]}}});
    let sorted_scan = json!({"sort": {"source": scan, "collation": [[0, "INTEGER", "ASCENDING NULLS LAST"]]}});
    let mut cases = vec![
        job(d, "same", json!({"schemas": schema, "queries": [scan, scan]}), None),
        // A bare ORDER BY is erased under bag semantics, so this is a real proof.
        job(d, "proved", json!({"schemas": schema, "queries": [scan, sorted_scan]}), None),
        job(d, "differs", json!({"schemas": schema, "queries": [scan, filtered]}), None),
        job(d, "refused", Value::Null, Some("unknown table t")),
        Case::new("no-job", "no-job"),
    ];
    let stats = run_second_opinion(&mut cases, d, &driver, 10_000);
    let got: Vec<(&str, &str)> =
        cases.iter().map(|c| (c.name.as_str(), c.s_bucket.as_deref().unwrap_or("-"))).collect();
    assert_eq!(
        got,
        [
            ("same", solver::PROVED_LITERAL),
            ("proved", solver::PROVED),
            ("differs", solver::NO_PROOF),
            ("refused", solver::UNSUPPORTED),
            ("no-job", solver::UNSUPPORTED),
        ]
    );
    assert_eq!((stats.rows, stats.answered, stats.halts), (4, Some(4), 0));
    assert_eq!(cases[3].s_note, "unknown table t");
}

/// A portfolio asks sqleq-solver one job at a time, inside the case's deadline, with its own cap and
/// grace period; its answers must bucket exactly as the batched pass buckets them.
#[test]
fn a_portfolio_asks_it_per_case() {
    let Some(driver) = sqleq_solver() else {
        eprintln!("skipped: sqleq-solver is not built (set $SQLEQ_SOLVER_BIN to require it)");
        return;
    };
    let frontend = ["debug", "release"].iter().map(|p| repo().join("target").join(p).join("sqleq-frontend")).find(|b| is_exe(b));
    let required = std::env::var_os("SQLEQ_SOLVER_BIN").is_some();
    let Some(frontend) = frontend else {
        assert!(!required, "the portfolio test needs sqleq-frontend built beside sqleq-solver");
        eprintln!("skipped: sqleq-frontend is not built");
        return;
    };
    let frontend = frontend.to_string_lossy().into_owned();
    let axes = ["frontend", "sqleq-solver"];
    let ctx = portfolio::Ctx {
        axes: &axes,
        frontend: Some(&frontend),
        prover: None,
        ss: Some(&driver),
        ss_cap_ms: None,
        fuzz: None,
        lean: None,
        timeout: 30.0,
        smt_timeout_ms: None,
        keep_dir: None,
    };
    for (example, bucket, verdict) in [
        ("in_vs_or.sql", solver::PROVED_LITERAL, portfolio::EQUIVALENT),
        ("dropped_filter.sql", solver::NO_PROOF, portfolio::UNDECIDED),
    ] {
        let case = portfolio::run_case(&repo().join("examples").join(example), example, &ctx);
        let o = case.portfolio.as_ref().unwrap();
        assert_eq!((case.s_bucket.as_deref(), o.verdict.as_str()), (Some(bucket), verdict), "{example}: {}", case.s_note);
        assert!(o.done.contains_key("sqleq-solver") && o.pending.is_empty(), "{example}: {o:?}");
    }
}
