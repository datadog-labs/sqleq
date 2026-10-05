// Unless explicitly stated otherwise all files in this repository are licensed under the
// Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/).
// Copyright 2026-Present Datadog, Inc.

//! Stand-ins for the backend binaries, as `/bin/sh` scripts, and a pair-file builder.

#![allow(dead_code)]

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::RwLock;

/// Writing a script and running one exclude each other within this test process. A process forked
/// by another test thread while a script is open for writing holds that file open until it execs,
/// and running the script in that window fails with `ETXTBSY` ("Text file busy"). Writers take it
/// exclusively, spawners shared.
static EXEC: RwLock<()> = RwLock::new(());

/// Run `f`, which starts subprocesses, while no stand-in is being written.
pub fn spawning<T>(f: impl FnOnce() -> T) -> T {
    let _shared = EXEC.read().unwrap_or_else(|e| e.into_inner());
    f()
}

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
    let _exclusive = EXEC.write().unwrap_or_else(|e| e.into_inner());
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
case " $* " in *" --csv "*)
    out=""; rep=""; r0="pair$(printf '%04d' 0)"
    while [ $# -gt 0 ]; do
        case "$1" in -o) out=$2; shift ;; --report) rep=$2; shift ;; esac
        shift
    done
    mkdir -p "$out"
    case "${FAKE_FE_STATUS:-emit}" in
    emit)
        printf '{"schemas": [], "queries": [{"scan": 0}, {"scan": 1}]}' > "$out/$r0.json"
        printf '{"detail": [{"row": 0, "name": "%s", "status": "emit"}]}' "$r0" > "$rep" ;;
    *)
        printf '{"detail": [{"row": 0, "name": "%s", "status": "%s", "kind": "unsupported", "reason": "unsupported: LIMIT"}]}' "$r0" "$FAKE_FE_STATUS" > "$rep" ;;
    esac
    exit 0 ;;
esac
pos=""
for a in "$@"; do case "$a" in --*) ;; *) pos="$pos $a" ;; esac; done
set -- $pos
printf '{"schemas": [], "queries": [{"scan": 0}, {"scan": 1}]}' > "$2"
"#;

/// Answers `$FAKE_SS` for every job, after `$FAKE_SS_SLEEP` seconds when that is set; kills itself
/// on reaching the job named `$FAKE_SS_DIE_ON`, as a driver out of memory would die.
pub const FAKE_SOLVER: &str = r#"[ -n "$FAKE_SS_SLEEP" ] && sleep "$FAKE_SS_SLEEP"
while IFS= read -r line || [ -n "$line" ]; do
    name=$(printf '%s' "$line" | sed -n 's/.*"name": *"\([^"]*\)".*/\1/p')
    [ -n "$FAKE_SS_DIE_ON" ] && [ "$name" = "$FAKE_SS_DIE_ON" ] && kill -9 $$
    printf '{"name": "%s", "verdict": "%s", "ms": 1}\n' "$name" "$FAKE_SS" >> "$2"
done < "$1"
"#;

/// Prints `$FAKE_FUZZ`, after `$FAKE_FUZZ_SLEEP` seconds when that is set, then `partial:
/// $FAKE_FUZZ_PARTIAL` when that is set; appends its arguments to `$FAKE_FUZZ_ARGV`.
pub const FAKE_FUZZ: &str = r#"[ -n "$FAKE_FUZZ_ARGV" ] && printf '%s\n' "$*" >> "$FAKE_FUZZ_ARGV"
[ -n "$FAKE_FUZZ_SLEEP" ] && sleep "$FAKE_FUZZ_SLEEP"
echo "$FAKE_FUZZ"
[ -n "$FAKE_FUZZ_PARTIAL" ] && echo "partial: $FAKE_FUZZ_PARTIAL"
exit 0
"#;

/// Writes `<stem>.result` saying `provable` when `$FAKE_QED` is `proved`. Appends its pid to
/// `$FAKE_PIDS`, sleeps `$FAKE_QED_SLEEP` seconds when that is set, and 30 seconds the first time
/// it runs when `$FAKE_QED_ONCE` names a file that does not exist yet.
pub const FAKE_PROVER: &str = r#"for a in "$@"; do json=$a; done
[ -n "$FAKE_PIDS" ] && echo $$ >> "$FAKE_PIDS"
[ -n "$FAKE_ULIMIT" ] && ulimit -v >> "$FAKE_ULIMIT"
if [ -n "$FAKE_QED_ONCE" ] && [ ! -e "$FAKE_QED_ONCE" ]; then : > "$FAKE_QED_ONCE"; sleep 30; fi
[ -n "$FAKE_QED_SLEEP" ] && sleep "$FAKE_QED_SLEEP"
case "$FAKE_QED" in proved) p=true ;; *) p=false ;; esac
printf '{"provable": %s, "panicked": false}' "$p" > "${json%.json}.result"
"#;

/// Answers `$FAKE_LEAN` for every pair file it is given, or under `--csv` for every name in
/// `--names` (row 0's name without one).
pub const FAKE_LEAN: &str = r#"out=""; prev=""; body=""; sep=""; csv=""; names=""
for a in "$@"; do
    [ "$prev" = "--json" ] && out=$a
    [ "$prev" = "--csv" ] && csv=$a
    [ "$prev" = "--names" ] && names=$a
    case "$a" in *.sql) body="$body$sep\"$a\": {\"verdict\": \"$FAKE_LEAN\", \"ms\": 1}"; sep=", " ;; esac
    prev=$a
done
if [ -n "$csv" ]; then
    if [ -n "$names" ]; then list=$(cat "$names"); else list="pair$(printf '%04d' 0)"; fi
    for n in $list; do body="$body$sep\"$n\": {\"verdict\": \"$FAKE_LEAN\", \"ms\": 1, \"generated\": true}"; sep=", "; done
fi
printf '{%s}' "$body" > "$out"
"#;

pub struct Fakes {
    pub dir: sqleq_check::util::TempDir,
    pub frontend: PathBuf,
    pub solver: PathBuf,
    pub fuzz: PathBuf,
    pub prover: PathBuf,
    pub lean: PathBuf,
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
            prover: exe(&d.join("qed"), FAKE_PROVER),
            lean: exe(&d.join("ln"), FAKE_LEAN),
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
    let Output { status, stdout, stderr } = spawning(|| cmd.output().unwrap());
    Ran {
        code: status.code().unwrap_or(-1),
        out: String::from_utf8_lossy(&stdout).into_owned(),
        err: String::from_utf8_lossy(&stderr).into_owned(),
    }
}

pub fn s(v: &str) -> String {
    v.to_string()
}
