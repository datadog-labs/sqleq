// Unless explicitly stated otherwise all files in this repository are licensed under the
// Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/).
// Copyright 2026-Present Datadog, Inc.

//! The binary end to end, over stand-in backends.
//!
//! The load-bearing tests are the controls: a fake prover that proves a non-equivalent pair must
//! fail the run, with and without `--bless`, and must leave the file alone. A suite whose invariant
//! check had stopped firing would pass every other test here.

mod common;

use std::path::Path;

use common::*;
use sqleq_check::case::Case;

struct Run<'a> {
    f: &'a Fakes,
    ss: &'a str,
    fuzz: &'a str,
}

impl Run<'_> {
    fn main(&self, extra: &[&str], axes: &str) -> Ran {
        let f = self.f;
        let mut argv: Vec<String> = ["--expect", "pinned", "--axes", axes, "-j", "1"].iter().map(|x| s(x)).collect();
        for (flag, bin) in [("--frontend", &f.frontend), ("--sqleq-solver-bin", &f.solver), ("--fuzz-bin", &f.fuzz)] {
            argv.extend([s(flag), bin.to_string_lossy().into_owned()]);
        }
        argv.extend(extra.iter().map(|x| s(x)));
        argv.push(f.case.to_string_lossy().into_owned());
        self.raw(f.path(), &argv)
    }

    fn raw(&self, cwd: &Path, argv: &[String]) -> Ran {
        let log = self.f.argv_log.to_string_lossy().into_owned();
        run(cwd, argv, &[("FAKE_ARGV", &log), ("FAKE_SS", self.ss), ("FAKE_FUZZ", self.fuzz)])
    }
}

const FE_SS: &str = "frontend,sqleq-solver";

fn read(p: &Path) -> String {
    std::fs::read_to_string(p).unwrap()
}

#[test]
fn a_false_proof_fails_and_bless_will_not_pin_it() {
    let f = Fakes::new();
    let r = Run { f: &f, ss: "EQ", fuzz: "NO-COUNTEREXAMPLE" };
    let text = pair(&NEQ_OK, &["-- expect frontend: emit"]);
    std::fs::write(&f.case, &text).unwrap();
    let ran = r.main(&[], FE_SS);
    assert_eq!(ran.code, 1);
    assert!(ran.out.contains("‼ proved"), "{}", ran.out);
    assert_eq!(r.main(&["--bless"], FE_SS).code, 1, "a bless run that leaves an invariant fails");
    assert_eq!(read(&f.case), text, "bless must not touch an invariant");

    // A person marks it known: the run passes while the bug reproduces ...
    std::fs::write(
        &f.case,
        pair(&NEQ_OK, &["-- expect frontend: emit", "-- expect sqleq-solver: proved !known-unsound"]),
    )
    .unwrap();
    assert_eq!(r.main(&[], FE_SS).code, 0);
    // ... and fails the run that fixes it, until bless drops the marker.
    let fixed = Run { ss: "NEQ", ..r };
    let ran = fixed.main(&[], FE_SS);
    assert_eq!(ran.code, 1);
    assert!(ran.out.contains("no longer reproduces"), "{}", ran.out);
    assert_eq!(fixed.main(&["--bless"], FE_SS).code, 0);
    assert!(read(&f.case).contains("-- expect sqleq-solver: no-proof\n"));
    assert!(!read(&f.case).contains("!known-unsound"));
}

#[test]
fn the_old_axis_name_is_read_and_a_rewritten_pin_gets_the_new_one() {
    let f = Fakes::new();
    let r = Run { f: &f, ss: "NEQ", fuzz: "NO-COUNTEREXAMPLE" };
    // `sqlsolver-rust` was sqleq-solver's axis before the rename: a pin under it still holds ...
    std::fs::write(&f.case, pair(&NEQ_OK, &["-- expect frontend: emit", "-- expect sqlsolver-rust: no-proof"]))
        .unwrap();
    assert_eq!(r.main(&[], "frontend,sqlsolver-rust").code, 0);
    assert_eq!(r.main(&[], FE_SS).code, 0);
    // ... and once it moves, `--bless` writes it back under the new name.
    let r = Run { ss: "UNKNOWN", ..r };
    std::fs::write(&f.case, pair(&NEQ_OK, &["-- expect frontend: emit", "-- expect sqlsolver-rust: unsupported"]))
        .unwrap();
    assert_eq!(r.main(&[], FE_SS).code, 1);
    assert_eq!(r.main(&["--bless"], FE_SS).code, 0);
    assert!(read(&f.case).contains("-- expect sqleq-solver: no-proof\n"));
    assert!(!read(&f.case).contains("sqlsolver-rust"));
}

#[test]
fn a_false_refutation_fails() {
    let f = Fakes::new();
    let r = Run { f: &f, ss: "NEQ", fuzz: "NOT-EQUIVALENT" };
    std::fs::write(&f.case, pair(&EQ_OK, &[])).unwrap();
    assert_eq!(r.main(&["--bless"], "fuzz").code, 1);
    assert!(!read(&f.case).contains("-- expect fuzz:"));
}

#[test]
fn bless_round_trip() {
    let f = Fakes::new();
    let r = Run { f: &f, ss: "NEQ", fuzz: "NO-COUNTEREXAMPLE" };
    let all = "frontend,fuzz,sqleq-solver";
    std::fs::write(&f.case, pair(&NEQ_OK, &[])).unwrap();
    assert_eq!(r.main(&[], all).code, 1, "unpinned");
    assert_eq!(r.main(&["--bless"], all).code, 0);
    let blessed = read(&f.case);
    for line in ["-- expect frontend: emit\n", "-- expect fuzz: no-counterexample\n", "-- expect sqleq-solver: no-proof\n"] {
        assert!(blessed.contains(line), "{line} in {blessed}");
    }
    assert_eq!(r.main(&[], all).code, 0);
    let ran = r.main(&["--bless"], all);
    assert_eq!(ran.code, 0);
    assert_eq!(read(&f.case), blessed);
    assert!(ran.out.contains("blessed 0 file(s)"), "{}", ran.out);
}

#[test]
fn an_improvement_fails_until_blessed() {
    let f = Fakes::new();
    let r = Run { f: &f, ss: "EQ", fuzz: "NO-COUNTEREXAMPLE" };
    std::fs::write(&f.case, pair(&EQ_OK, &["-- expect frontend: emit", "-- expect sqleq-solver: no-proof"])).unwrap();
    let ran = r.main(&[], FE_SS);
    assert_eq!(ran.code, 1);
    assert!(ran.out.contains("no-proof→proved"), "{}", ran.out);
}

#[test]
fn a_lint_error_fails_and_bless_skips_the_file() {
    let f = Fakes::new();
    let r = Run { f: &f, ss: "NEQ", fuzz: "NO-COUNTEREXAMPLE" };
    let text = pair(&["-- truth: not-equivalent", "-- origin: x"], &[]);
    std::fs::write(&f.case, &text).unwrap();
    assert_eq!(r.main(&["--bless"], FE_SS).code, 1);
    assert_eq!(read(&f.case), text);
}

/// Each case runs in its own working directory, so a relative `--frontend` used to name nothing
/// there. CI passes `target/debug/...`, which is how this was found.
#[test]
fn relative_binary_paths_work() {
    let f = Fakes::new();
    let r = Run { f: &f, ss: "NEQ", fuzz: "NO-COUNTEREXAMPLE" };
    std::fs::write(
        &f.case,
        pair(
            &NEQ_OK,
            &["-- expect frontend: emit", "-- expect fuzz: no-counterexample", "-- expect sqleq-solver: no-proof"],
        ),
    )
    .unwrap();
    let argv: Vec<String> = [
        "--expect",
        "pinned",
        "--axes",
        "frontend,fuzz,sqleq-solver",
        "-j",
        "1",
        "--frontend",
        "fe",
        "--sqleq-solver-bin",
        "ss",
        "--fuzz-bin",
        "fz",
        "case.sql",
    ]
    .iter()
    .map(|x| s(x))
    .collect();
    let ran = r.raw(f.path(), &argv);
    assert_eq!(ran.code, 0, "{}{}", ran.out, ran.err);
}

#[test]
fn the_catalog_header_reaches_the_frontend() {
    let f = Fakes::new();
    let r = Run { f: &f, ss: "NEQ", fuzz: "NO-COUNTEREXAMPLE" };
    std::fs::write(&f.case, pair(&NEQ_OK, &["-- catalog: inferred-seeded"])).unwrap();
    r.main(&[], FE_SS);
    let first = read(&f.argv_log).lines().next().unwrap_or("").to_string();
    assert!(first.split(' ').any(|a| a == "--infer-seeded"), "{first}");
}

#[test]
fn fuzz_alone_needs_no_frontend() {
    let f = Fakes::new();
    let r = Run { f: &f, ss: "NEQ", fuzz: "NO-COUNTEREXAMPLE" };
    std::fs::write(&f.case, pair(&NEQ_OK, &["-- expect fuzz: no-counterexample"])).unwrap();
    assert_eq!(r.main(&[], "fuzz").code, 0);
    assert!(!f.argv_log.exists());
}

/// A missing tool or a bad flag is exit 2, never 1: it is not a case that failed.
#[test]
fn setup_errors_exit_2() {
    let f = Fakes::new();
    let r = Run { f: &f, ss: "NEQ", fuzz: "NO-COUNTEREXAMPLE" };
    std::fs::write(&f.case, pair(&NEQ_OK, &[])).unwrap();
    let plan = f.path().join("plan.json");
    std::fs::write(&plan, "{}").unwrap();
    let nope = f.path().join("nope").to_string_lossy().into_owned();
    let rows: Vec<(Vec<&str>, &Path)> = vec![
        (vec!["--expect", "pinned", "--axes", "fuzz", "--fuzz-bin", &nope], &f.case),
        (vec!["--expect", "pinned", "--axes", "frontend,qd"], &f.case),
        (vec!["--expect", "pinned", "--axes", "sqleq-solver,sqlsolver-jvm"], &f.case),
        (vec!["--expect", "equivalent", "--axes", "frontend"], &f.case),
        (vec!["--expect", "report-only", "--bless"], &f.case),
        (vec!["--expect", "pinned", "--axes", "frontend"], &plan),
        (vec!["--sqleq-solver", "--sqlsolver-jvm"], &f.case),
        (vec!["--jobs", "many"], &f.case),
    ];
    for (argv, path) in rows {
        let mut a: Vec<String> = argv.iter().map(|x| s(x)).collect();
        a.extend([s("--frontend"), f.frontend.to_string_lossy().into_owned(), path.to_string_lossy().into_owned()]);
        let ran = r.raw(f.path(), &a);
        assert_eq!(ran.code, 2, "{argv:?}: {}{}", ran.out, ran.err);
    }
}

#[test]
fn fuzz_labels_map_to_their_kind() {
    let f = Fakes::new();
    let case = f.path().join("c.sql");
    std::fs::write(&case, "").unwrap();
    let rows = [
        ("echo NOT-EQUIVALENT; echo 'counterexample: t=[(0)]'", ("counterexample", "t=[(0)]")),
        ("echo NO-COUNTEREXAMPLE", ("no-counterexample", "")),
        ("echo 'PARAM-MISALIGNED:$1 vs $2'", ("param-misaligned", "$1 vs $2")),
        ("echo 'ERROR:Binder Error'", ("error", "Binder Error")),
        ("echo boom >&2; exit 1", ("error", "boom")),
    ];
    for (i, (body, (word, note))) in rows.iter().enumerate() {
        let fake = exe(&f.path().join(format!("fz{i}")), &format!("{body}\n"));
        let (w, n, _) =
            spawning(|| sqleq_check::axes::fuzz::fuzz_one(&fake.to_string_lossy(), &case.to_string_lossy(), 10.0));
        assert_eq!((w.as_str(), n.as_str()), (*word, *note), "{body}");
    }
}

/// `--lean` attaches sqleq-lean's verdicts to the right cases, by path.
#[test]
fn lean_verdicts_attach_by_path_and_json_cases_are_skipped() {
    let f = Fakes::new();
    // A stand-in: answers `proved-gather` for a path ending in a.sql, and says nothing at all about
    // any other, which the harness must report as `missing`.
    let fake = exe(
        &f.path().join("sqleq-lean"),
        r#"out=""; prev=""; body=""
for a in "$@"; do
    [ "$prev" = "--json" ] && out=$a
    case "$a" in *a.sql) body="$body\"$a\": {\"verdict\": \"proved-gather\", \"shape\": \"row-major\", \"ms\": 3}" ;; esac
    prev=$a
done
printf '{%s}' "$body" > "$out"
"#,
    );
    let at = |rel: &str| f.path().join(rel).to_string_lossy().into_owned();
    let mut cases = vec![Case::new("dir1/a.sql", &at("dir1/a.sql")), Case::new("dir2/b.sql", &at("dir2/b.sql")), Case::new("p.json", &at("p.json"))];
    let stats = spawning(|| sqleq_check::axes::lean::run_lean(&mut cases, &fake.to_string_lossy(), 2, 10.0, None));
    assert_eq!(stats.rows, 2);
    let a = &cases[0];
    assert_eq!(
        (a.l_verdict.as_deref(), a.l_shape.as_str(), a.l_ms.clone()),
        (Some("proved-gather"), "row-major", Some(serde_json::json!(3)))
    );
    assert_eq!(cases[1].l_verdict.as_deref(), Some("missing"));
    assert_eq!(cases[2].l_verdict, None, "a pre-parsed .json plan has no pair file to read");
}
