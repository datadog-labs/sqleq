// Unless explicitly stated otherwise all files in this repository are licensed under the
// Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/).
// Copyright 2026-Present Datadog, Inc.

//! The executable's own behaviour: `csv` mode writes beside its input (issue #67).

use std::path::{Path, PathBuf};
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_sqleq-fuzz");

/// A fresh directory under the target directory, so nothing is written outside the build tree.
fn scratch(name: &str) -> PathBuf {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("cli-{name}"));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

const DDL: &str = "create table t (id INTEGER, a INTEGER, unique (id))";

fn corpus(dir: &Path) -> PathBuf {
    let rows = [
        ("SELECT id FROM t WHERE a = 1", "SELECT id FROM t WHERE 1 = a"),
        ("SELECT id FROM t", "SELECT id FROM t WHERE a > 0"),
    ];
    let mut w = csv::Writer::from_path(dir.join("corpus.csv")).unwrap();
    for (a, b) in rows {
        w.write_record([a, b, DDL]).unwrap();
    }
    w.flush().unwrap();
    std::fs::write(dir.join("names.txt"), "row0\nrow1\n").unwrap();
    dir.join("corpus.csv")
}

fn verdicts(path: &Path) -> serde_json::Map<String, serde_json::Value> {
    let text = std::fs::read_to_string(path).unwrap();
    match serde_json::from_str(&text).unwrap() {
        serde_json::Value::Object(m) => m,
        other => panic!("{other}"),
    }
}

/// Without an output path the verdicts land beside the corpus, not at one fixed path every run
/// shares.
#[test]
fn csv_mode_writes_beside_its_input_by_default() {
    let dir = scratch("default-out");
    let corpus = corpus(&dir);
    let st = Command::new(BIN)
        .args(["csv"])
        .arg(&corpus)
        .arg(dir.join("names.txt"))
        .args(["--trials", "5"])
        .output()
        .unwrap();
    assert!(st.status.success(), "{st:?}");
    assert_eq!(verdicts(&dir.join("corpus.fuzz.json")).len(), 2);
}
