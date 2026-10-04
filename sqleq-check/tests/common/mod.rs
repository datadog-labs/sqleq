// Unless explicitly stated otherwise all files in this repository are licensed under the
// Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/).
// Copyright 2026-Present Datadog, Inc.

//! Stand-ins for the backend binaries, as `/bin/sh` scripts, and a pair-file builder.

#![allow(dead_code)]

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

pub const LICENCE: &str = "-- Unless explicitly stated otherwise all files in this repository are licensed under the
-- Apache License Version 2.0.
-- This product includes software developed at Datadog (https://www.datadoghq.com/).
-- Copyright 2026-Present Datadog, Inc.
";
pub const SQL: &str =
    "create table \"t\" (\"a\" INTEGER);\nSELECT \"a\" FROM \"t\";\nSELECT \"a\" FROM \"t\" WHERE \"a\" = 1;\n";

pub const NEQ_OK: [&str; 3] = ["-- truth: not-equivalent", "-- origin: a test", "-- witness: t = {(0)}"];
pub const EQ_OK: [&str; 3] = ["-- truth: equivalent", "-- origin: a test", "-- argument: because"];

pub fn pair(base: &[&str], more: &[&str]) -> String {
    let mut s = format!("{LICENCE}\n");
    for d in base.iter().chain(more) {
        s.push_str(d);
        s.push('\n');
    }
    s.push('\n');
    s.push_str(SQL);
    s
}

/// Write an executable `/bin/sh` script.
pub fn exe(path: &Path, body: &str) -> PathBuf {
    std::fs::write(path, format!("#!/bin/sh\n{body}")).unwrap();
    let mut perm = std::fs::metadata(path).unwrap().permissions();
    perm.set_mode(0o755);
    std::fs::set_permissions(path, perm).unwrap();
    path.to_path_buf()
}

/// Lowers every pair to two different plans and records its arguments in `$FAKE_ARGV`; packages a
/// second-opinion job on `--sqlsolver`.
pub const FAKE_FRONTEND: &str = r#"printf '%s\n' "$*" >> "$FAKE_ARGV"
case " $* " in *" --sqlsolver "*)
    name=""; out=""
    while [ $# -gt 0 ]; do
        case "$1" in --name) name=$2; shift ;; -o) out=$2; shift ;; esac
        shift
    done
    printf '{"name": "%s", "ir": {"queries": [1, 2]}, "schema": ""}\n' "$name" > "$out"
    exit 0 ;;
esac
pos=""
for a in "$@"; do case "$a" in --*) ;; *) pos="$pos $a" ;; esac; done
set -- $pos
printf '{"schemas": [], "queries": [{"scan": 0}, {"scan": 1}]}' > "$2"
"#;

/// Answers `$FAKE_SS` for every job.
pub const FAKE_SOLVER: &str = r#"while IFS= read -r line || [ -n "$line" ]; do
    name=$(printf '%s' "$line" | sed -n 's/.*"name": *"\([^"]*\)".*/\1/p')
    printf '{"name": "%s", "verdict": "%s", "ms": 1}\n' "$name" "$FAKE_SS" >> "$2"
done < "$1"
"#;

/// Prints `$FAKE_FUZZ`.
pub const FAKE_FUZZ: &str = "echo \"$FAKE_FUZZ\"\n";

pub struct Fakes {
    pub dir: sqleq_check::util::TempDir,
    pub frontend: PathBuf,
    pub solver: PathBuf,
    pub fuzz: PathBuf,
    pub argv_log: PathBuf,
    pub case: PathBuf,
}

impl Fakes {
    pub fn new() -> Fakes {
        let dir = sqleq_check::util::TempDir::new("sqleq-check-test-").unwrap();
        let d = dir.path().to_path_buf();
        Fakes {
            frontend: exe(&d.join("fe"), FAKE_FRONTEND),
            solver: exe(&d.join("ss"), FAKE_SOLVER),
            fuzz: exe(&d.join("fz"), FAKE_FUZZ),
            argv_log: d.join("argv.log"),
            case: d.join("case.sql"),
            dir,
        }
    }

    pub fn path(&self) -> &Path {
        self.dir.path()
    }
}

pub struct Ran {
    pub code: i32,
    pub out: String,
    pub err: String,
}

/// Run the real binary with the fakes' environment.
pub fn run(cwd: &Path, argv: &[String], env: &[(&str, &str)]) -> Ran {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_sqleq-check"));
    cmd.args(argv).current_dir(cwd);
    for (k, v) in env {
        cmd.env(k, v);
    }
    let Output { status, stdout, stderr } = cmd.output().unwrap();
    Ran {
        code: status.code().unwrap_or(-1),
        out: String::from_utf8_lossy(&stdout).into_owned(),
        err: String::from_utf8_lossy(&stderr).into_owned(),
    }
}

pub fn s(v: &str) -> String {
    v.to_string()
}
