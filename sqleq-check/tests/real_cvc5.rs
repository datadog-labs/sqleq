// Unless explicitly stated otherwise all files in this repository are licensed under the
// Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/).
// Copyright 2026-Present Datadog, Inc.

//! The cvc5 axis end to end against the real `sqleq-cvc5`: the driver reads a job, writes its row,
//! and the harness buckets it -- a proof, an exact model, a refusal.
//!
//! sqleq-cvc5 needs a cvc5 library to build, so this skips when no build of it is found -- unless
//! `$SQLEQ_CVC5_BIN` names one, when a missing binary is a failure.

use serde_json::json;
use sqleq_check::axes::cvc5::{self, run_cvc5};
use sqleq_check::case::{ss_slug, Case};
use sqleq_check::discover::{repo, Cvc5Driver};
use sqleq_check::util::{is_exe, TempDir};

fn sqleq_cvc5() -> Option<Cvc5Driver> {
    let bin = match std::env::var("SQLEQ_CVC5_BIN").ok().filter(|b| !b.is_empty()) {
        Some(b) => {
            let p = repo().join(&b);
            assert!(is_exe(&p), "$SQLEQ_CVC5_BIN={b} is not an executable");
            p
        }
        None => ["release", "debug"].iter().map(|p| repo().join("target").join(p).join("sqleq-cvc5")).find(|b| is_exe(b))?,
    };
    let bin = bin.canonicalize().unwrap().to_string_lossy().into_owned();
    Some(Cvc5Driver { bin, semantics: "bags".into(), options: Vec::new() })
}

#[test]
fn rows_are_bucketed() {
    let Some(driver) = sqleq_cvc5() else {
        eprintln!("skipped: sqleq-cvc5 is not built (set $SQLEQ_CVC5_BIN to require it)");
        return;
    };
    let dir = TempDir::new("sqleq-cvc5-test-").unwrap();
    let d = dir.path();
    let schema = json!([{"name": "t", "types": ["INTEGER"], "key": [], "nullable": [true]}]);
    let scan = json!({"scan": 0});
    let one = json!({"operator": "1", "operand": [], "type": "INTEGER"});
    let col = json!({"column": 0, "type": "INTEGER"});
    let filtered = json!({"filter": {"source": scan, "condition": {"operator": "=", "type": "BOOLEAN", "operand": [col, one]}}});
    let sorted = json!({"sort": {"source": scan, "collation": [[0, "INTEGER", "ASCENDING NULLS LAST"]]}});
    let limited = json!({"sort": {"source": scan, "collation": [], "limit": one}});
    let mut cases = Vec::new();
    for (name, ir) in [
        ("same", json!({"schemas": schema, "queries": [scan, scan]})),
        ("proved", json!({"schemas": schema, "queries": [scan, sorted]})),
        ("differs", json!({"schemas": schema, "queries": [scan, filtered]})),
        ("refused", json!({"schemas": schema, "queries": [scan, limited]})),
    ] {
        let plan = d.join(format!("{name}.json"));
        std::fs::write(&plan, ir.to_string()).unwrap();
        cvc5::write_job(name, &plan, &d.join(format!("{}.job.jsonl", ss_slug(name)))).unwrap();
        cases.push(Case::new(name, name));
    }
    cases.push(Case::new("no-job", "no-job"));
    let stats = run_cvc5(&mut cases, d, &driver, 2, Some(20_000), 30.0, &|_| {});
    let got: Vec<(&str, &str)> = cases.iter().map(|c| (c.name.as_str(), c.c_bucket.as_deref().unwrap_or("-"))).collect();
    assert_eq!(
        got,
        [
            ("same", cvc5::PROVED_LITERAL),
            ("proved", cvc5::PROVED),
            ("differs", cvc5::MODEL),
            ("refused", cvc5::UNSUPPORTED),
            ("no-job", cvc5::UNSUPPORTED),
        ]
    );
    assert_eq!(stats.rows, 4);
    assert!(cases[2].c_note.starts_with("t=["), "{}", cases[2].c_note);
    assert_eq!(cases[3].c_note, "unsupported-sort");
}
