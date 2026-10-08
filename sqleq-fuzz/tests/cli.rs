// Unless explicitly stated otherwise all files in this repository are licensed under the
// Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/).
// Copyright 2026-Present Datadog, Inc.

//! The executable's own behaviour: `csv` mode keeps every row and writes beside its input
//! (issues #63, #67), `file` mode reads an `ALTER TABLE` as DDL (issue #64), and the engine is
//! Postgres unless it is told otherwise. Each test runs on both engines, Postgres only where a
//! PostgreSQL 17 is there to run on (always, when `$SQLEQ_PG_REQUIRED` says one must be).

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

/// The engines to test on: DuckDB, and Postgres where it can run.
fn engines() -> Vec<&'static str> {
    let found = sqleq_fuzz::pg::bin_dir().and_then(|b| sqleq_fuzz::pg::version(&b).map(|_| ()));
    match found {
        Ok(()) => vec!["duckdb", "postgres"],
        Err(_) if std::env::var_os("SQLEQ_PG_REQUIRED").is_some() => vec!["duckdb", "postgres"],
        Err(e) => {
            eprintln!("postgres engine skipped: {e}");
            vec!["duckdb"]
        }
    }
}

const DDL: &str = "create table t (id INTEGER, a INTEGER, unique (id))";

fn corpus(dir: &Path) -> PathBuf {
    let rows = [
        ("SELECT id FROM t WHERE a = 1", "SELECT id FROM t WHERE 1 = a"),
        (
            "SELECT id FROM t WHERE a = $4294967296",
            "SELECT id FROM t WHERE a = $4294967296",
        ),
        ("SELECT id FROM t", "SELECT id FROM t WHERE a > 0"),
    ];
    let mut w = csv::Writer::from_path(dir.join("corpus.csv")).unwrap();
    for (a, b) in rows {
        w.write_record([a, b, DDL]).unwrap();
    }
    w.flush().unwrap();
    std::fs::write(dir.join("names.txt"), "row0\nrow1\nrow2\n").unwrap();
    dir.join("corpus.csv")
}

fn verdicts(path: &Path) -> serde_json::Map<String, serde_json::Value> {
    let text = std::fs::read_to_string(path).unwrap();
    match serde_json::from_str(&text).unwrap() {
        serde_json::Value::Object(m) => m,
        other => panic!("{other}"),
    }
}

/// One row that cannot be tested must not cost the others their verdicts. With one worker the row
/// after it used to be missing, and the run still exited 0.
#[test]
fn csv_mode_gives_every_row_a_verdict() {
    for engine in engines() {
        for jobs in ["1", "2"] {
            let dir = scratch(&format!("rows-{engine}-{jobs}"));
            let corpus = corpus(&dir);
            let out = dir.join("out.json");
            let st = Command::new(BIN)
                .args(["csv"])
                .arg(&corpus)
                .arg(dir.join("names.txt"))
                .arg(&out)
                .args(["--jobs", jobs, "--trials", "20", "--engine", engine])
                .output()
                .unwrap();
            assert!(st.status.success(), "{engine}: {st:?}");
            let v = verdicts(&out);
            assert_eq!(v.len(), 3, "{engine}, jobs {jobs}: {v:?}");
            let label = |k: &str| v[k]["verdict"].as_str().unwrap().to_string();
            assert!(label("row1").starts_with("ERROR:"), "{engine}: {v:?}");
            assert_eq!(label("row2"), "NOT-EQUIVALENT", "{engine}");
        }
    }
}

/// Without an output path the verdicts land beside the corpus, not at one fixed path every run
/// shares.
#[test]
fn csv_mode_writes_beside_its_input_by_default() {
    for engine in engines() {
        let dir = scratch(&format!("default-out-{engine}"));
        let corpus = corpus(&dir);
        let st = Command::new(BIN)
            .args(["csv"])
            .arg(&corpus)
            .arg(dir.join("names.txt"))
            .args(["--trials", "5", "--engine", engine])
            .output()
            .unwrap();
        assert!(st.status.success(), "{engine}: {st:?}");
        assert_eq!(verdicts(&dir.join("corpus.fuzz.json")).len(), 3, "{engine}");
    }
}

/// `ALTER TABLE` belongs to the schema, so a pair file carrying one has two statements, not three.
#[test]
fn file_mode_reads_alter_table_as_ddl() {
    for engine in engines() {
        let dir = scratch(&format!("alter-{engine}"));
        let pair = alter_pair(&dir);
        let st = Command::new(BIN)
            .arg("file")
            .arg(&pair)
            .args(["--engine", engine])
            .output()
            .unwrap();
        assert!(st.status.success(), "{engine}: {st:?}");
        assert_eq!(
            String::from_utf8_lossy(&st.stdout).lines().next(),
            Some("NO-COUNTEREXAMPLE"),
            "{engine}"
        );
    }
}

fn alter_pair(dir: &Path) -> PathBuf {
    let pair = dir.join("pair.sql");
    std::fs::write(
        &pair,
        "-- a pair\ncreate table t (id INTEGER NOT NULL, a INTEGER);\n\
         alter table t add primary key (id);\n\
         SELECT id FROM t;\nSELECT DISTINCT id FROM t;\n",
    )
    .unwrap();
    pair
}

/// With no `--engine` the pair runs on Postgres; `$SQLEQ_FUZZ_ENGINE` picks DuckDB. Each says which.
#[test]
fn the_engine_is_postgres_unless_told_otherwise() {
    if !engines().contains(&"postgres") {
        return;
    }
    let dir = scratch("default-engine");
    let pair = alter_pair(&dir);
    let run = |env: Option<&str>| {
        let mut cmd = Command::new(BIN);
        cmd.arg("file").arg(&pair).env_remove("SQLEQ_FUZZ_ENGINE");
        if let Some(e) = env {
            cmd.env("SQLEQ_FUZZ_ENGINE", e);
        }
        let st = cmd.output().unwrap();
        assert!(st.status.success(), "{st:?}");
        String::from_utf8_lossy(&st.stdout).into_owned()
    };
    let default = run(None);
    assert!(
        default.lines().any(|l| l.starts_with("engine: postgres 17.")),
        "{default}"
    );
    let duck = run(Some("duckdb"));
    assert!(duck.lines().any(|l| l.starts_with("engine: duckdb ")), "{duck}");
}
