// Unless explicitly stated otherwise all files in this repository are licensed under the
// Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/).
// Copyright 2026-Present Datadog, Inc.

//! The command line: each pair file gets a record of its own. Runs no Lean (`--translate-only`).

use std::path::PathBuf;
use std::process::Command;

const PAIR: &str = "\
create table events (id bigint, kind text);
INSERT INTO events (id, kind) VALUES ($1, $2), ($3, $4);
INSERT INTO events (id, kind) SELECT * FROM unnest($1::bigint[], $2::text[]);
";

fn lean() -> Command {
    Command::new(env!("CARGO_BIN_EXE_sqleq-lean"))
}

#[test]
fn two_pair_files_with_one_name_are_not_one_record() {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!("lean-cli-{}", std::process::id()));
    let (d1, d2) = (dir.join("d1"), dir.join("d2"));
    for d in [&d1, &d2] {
        std::fs::create_dir_all(d).unwrap();
        std::fs::write(d.join("x.sql"), PAIR).unwrap();
    }
    // Keyed by file name, the second record would overwrite the first.
    let out = lean().arg("--translate-only").arg(&d1).arg(&d2).output().unwrap();
    assert_eq!(out.status.code(), Some(2), "stdout: {}", String::from_utf8_lossy(&out.stdout));
    assert!(String::from_utf8_lossy(&out.stderr).contains("--full-names"));
    // Keyed by path, both are kept.
    let out = lean().arg("--translate-only").arg("--full-names").arg(&d1).arg(&d2).output().unwrap();
    assert_eq!(out.status.code(), Some(0), "stderr: {}", String::from_utf8_lossy(&out.stderr));
    let records: serde_json::Map<String, serde_json::Value> = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(records.len(), 2, "{records:?}");
    std::fs::remove_dir_all(&dir).unwrap();
}
