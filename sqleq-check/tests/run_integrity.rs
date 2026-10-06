// Unless explicitly stated otherwise all files in this repository are licensed under the
// Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/).
// Copyright 2026-Present Datadog, Inc.

//! That a run reports what its backends said in that run, over stand-in backends: a proof and a
//! counterexample on one pair fail every mode, a `--keep` re-run never reads a previous run's
//! answers, and a frontend crash is not a refusal.

mod common;

use std::path::{Path, PathBuf};

use common::*;
use serde_json::Value;

/// The stand-ins, any of which a test may swap for one of its own, and the environment they read.
struct Check<'a> {
    f: &'a Fakes,
    frontend: PathBuf,
    lean: PathBuf,
    env: Vec<(&'a str, String)>,
}

impl<'a> Check<'a> {
    fn new(f: &'a Fakes) -> Check<'a> {
        let mut c = Check { f, frontend: f.frontend.clone(), lean: f.lean.clone(), env: Vec::new() };
        c.set("FAKE_ARGV", &f.argv_log.to_string_lossy());
        c.set("FAKE_QED", "no-proof").set("FAKE_SS", "NEQ").set("FAKE_FUZZ", "NO-COUNTEREXAMPLE");
        c.set("FAKE_LEAN", "unsupported");
        c
    }

    fn set(&mut self, k: &'a str, v: &str) -> &mut Self {
        self.env.retain(|(x, _)| *x != k);
        self.env.push((k, v.to_string()));
        self
    }

    fn unset(&mut self, k: &str) -> &mut Self {
        self.env.retain(|(x, _)| *x != k);
        self
    }

    /// Run on the one case, from the fakes' directory, with `-j 1`.
    fn run(&self, extra: &[&str]) -> Ran {
        let f = self.f;
        let mut argv: Vec<String> = vec![s("-j"), s("1")];
        for (flag, bin) in [
            ("--frontend", &self.frontend),
            ("--prover", &f.prover),
            ("--sqleq-solver-bin", &f.solver),
            ("--fuzz-bin", &f.fuzz),
            ("--lean-bin", &self.lean),
        ] {
            argv.extend([s(flag), bin.to_string_lossy().into_owned()]);
        }
        argv.extend(extra.iter().map(|x| s(x)));
        argv.push(f.case.to_string_lossy().into_owned());
        let env: Vec<(&str, &str)> = self.env.iter().map(|(k, v)| (*k, v.as_str())).collect();
        run(f.path(), &argv, &env)
    }

    /// Run with `--json`, and return the one case's record and the report's `meta`.
    fn case(&self, extra: &[&str]) -> (Ran, Value, Value) {
        let out = self.f.path().join("out.json");
        let _ = std::fs::remove_file(&out);
        let o = out.to_string_lossy().into_owned();
        let mut a: Vec<&str> = extra.to_vec();
        a.extend(["--json", &o]);
        let ran = self.run(&a);
        let v: Value = serde_json::from_str(&std::fs::read_to_string(&out).unwrap_or_default()).unwrap_or(Value::Null);
        (ran, v["cases"][0].clone(), v["meta"].clone())
    }
}

fn verdict(case: &Value) -> &str {
    case["portfolio"]["verdict"].as_str().unwrap_or("-")
}

fn neq_case() -> Fakes {
    let f = Fakes::new();
    std::fs::write(&f.case, pair(&NEQ_OK, &[])).unwrap();
    f
}

/// A stand-in frontend that runs `body` and then, unless that exits, the common one.
fn frontend(dir: &Path, name: &str, body: &str, f: &Fakes) -> PathBuf {
    exe(&dir.join(name), &format!("{body}\nexec \"{}\" \"$@\"\n", f.frontend.display()))
}

// --- 1. A proof and a counterexample outside --portfolio ------------------------------------------

#[test]
fn a_proof_and_a_counterexample_fail_a_run_without_portfolio_under_every_policy() {
    let f = neq_case();
    let mut c = Check::new(&f);
    c.set("FAKE_QED", "proved").set("FAKE_FUZZ", "NOT-EQUIVALENT");
    for policy in ["equivalent", "report-only"] {
        let (ran, case, meta) = c.case(&["--axes", "frontend,qed,fuzz", "--expect", policy]);
        assert_eq!(case["status"], "provable");
        assert_eq!(case["f_verdict"], "counterexample");
        assert_eq!(ran.code, 1, "--expect {policy}: an alarm fails the run\n{}{}", ran.out, ran.err);
        assert!(ran.out.contains("‼ alarm"), "{}", ran.out);
        assert!(ran.out.contains("fuzz: counterexample, qed: proved"), "{}", ran.out);
        assert_eq!(meta["alarms"], serde_json::json!(["case.sql"]));
    }

    // Any claim of equivalence meets the counterexample: the second prover's proof ...
    c.set("FAKE_QED", "no-proof").set("FAKE_SS", "EQ");
    let ran = c.run(&["--axes", "frontend,sqleq-solver,fuzz", "--expect", "report-only"]);
    assert_eq!(ran.code, 1, "{}{}", ran.out, ran.err);
    assert!(ran.out.contains("sqleq-solver: proved"), "{}", ran.out);
    // ... and the frontend's finding that the two sides are one query.
    c.set("FAKE_SS", "NEQ").set("FAKE_FE_STATUS", "reflexive").set("FAKE_FE_NOTE", sqleq_frontend::REFLEXIVE_NOTE);
    let ran = c.run(&["--axes", "frontend,fuzz", "--expect", "report-only"]);
    assert_eq!(ran.code, 1, "{}{}", ran.out, ran.err);
    assert!(ran.out.contains("frontend: reflexive"), "{}", ran.out);

    // The control: the counterexample alone is no alarm.
    c.unset("FAKE_FE_STATUS");
    let (ran, _, meta) = c.case(&["--axes", "frontend,qed,fuzz", "--expect", "report-only"]);
    assert_eq!(ran.code, 0, "{}{}", ran.out, ran.err);
    assert!(!ran.out.contains("alarm"), "{}", ran.out);
    assert!(meta.get("alarms").is_none(), "{meta}");
}

#[test]
fn a_pinned_alarm_fails_unless_its_wrong_side_is_pinned_known_unsound() {
    let f = Fakes::new();
    let mut c = Check::new(&f);
    c.set("FAKE_QED", "proved").set("FAKE_FUZZ", "NOT-EQUIVALENT");
    let axes = ["--expect", "pinned", "--axes", "frontend,qed,fuzz"];

    // Stated under the gather binding, the pair has no index-binding truth for the two answers to
    // contradict: every pin holds, and the alarm is what is left to fail it.
    let gather = pair(
        &["-- truth: equivalent", "-- binding: gather", "-- origin: a test", "-- argument: because"],
        &["-- expect frontend: emit", "-- expect fuzz: counterexample", "-- expect qed: proved"],
    );
    std::fs::write(&f.case, gather).unwrap();
    let ran = c.run(&axes);
    assert!(ran.out.contains("pinned 1/1 case(s) hold"), "{}", ran.out);
    assert_eq!(ran.code, 1, "{}{}", ran.out, ran.err);
    assert!(ran.out.contains("‼ alarm"), "{}", ran.out);

    // A known false proof, pinned as one, is that bug reproducing: the run passes as the pin does.
    let known = pair(&NEQ_OK, &["-- expect frontend: emit", "-- expect fuzz: counterexample", "-- expect qed: proved !known-unsound"]);
    std::fs::write(&f.case, known).unwrap();
    let ran = c.run(&axes);
    assert_eq!(ran.code, 0, "{}{}", ran.out, ran.err);
    assert!(ran.out.contains("(pinned !known-unsound, still reproducing)"), "{}", ran.out);
}

// --- 2-4. --keep re-runs ---------------------------------------------------------------------------

#[test]
fn a_keep_rerun_does_not_read_the_previous_runs_prover_result() {
    let f = neq_case();
    let mut c = Check::new(&f);
    let keep = f.path().join("keep");
    let k = keep.to_string_lossy().into_owned();
    let args = ["--axes", "frontend,qed", "--expect", "report-only", "--no-retry", "--keep", &k];
    c.set("FAKE_QED", "proved");
    let (_, case, _) = c.case(&args);
    assert_eq!(case["status"], "provable");
    std::fs::write(keep.join("case.sql").join("stray"), "").unwrap();

    // Dies before writing a `.result`, as a z3 null dereference does.
    c.set("FAKE_QED_SIGNAL", "SEGV");
    let (ran, case, _) = c.case(&args);
    assert_eq!(case["status"], "panic", "{}{}", ran.out, ran.err);
    assert_eq!(case["q_rc"], -11);
    assert!(!keep.join("case.sql").join("case.result").exists(), "a result this run did not write");
    assert!(!keep.join("case.sql").join("stray").exists(), "the case's directory is emptied first");
}

#[test]
fn a_portfolio_keep_rerun_does_not_read_the_previous_runs_solver_row_or_lean_record() {
    let f = neq_case();
    let mut c = Check::new(&f);
    let keep = f.path().join("keep").to_string_lossy().into_owned();
    let args = ["--portfolio", "--axes", "frontend,sqleq-solver", "--expect", "report-only", "--no-retry", "--keep", &keep];
    c.set("FAKE_SS", "EQ");
    let (_, case, _) = c.case(&args);
    assert_eq!((case["s_bucket"].as_str(), verdict(&case)), (Some("proved"), "equivalent"));

    // Dies before writing a row, as a worker that overflows its stack does.
    c.set("FAKE_SS_DIE_ON", "case.sql");
    let (ran, case, _) = c.case(&args);
    assert_eq!((case["s_bucket"].as_str(), verdict(&case)), (Some("error"), "undecided"), "{}{}", ran.out, ran.err);

    let lean = ["--portfolio", "--axes", "lean", "--expect", "report-only", "--no-retry", "--keep", &keep];
    c.set("FAKE_LEAN", "proved-gather");
    let (_, case, _) = c.case(&lean);
    assert_eq!((case["l_verdict"].as_str(), verdict(&case)), (Some("proved-gather"), "equivalent-gather"));
    c.lean = exe(&f.path().join("ln-abort"), "kill -ABRT $$\n");
    let (ran, case, _) = c.case(&lean);
    assert_eq!((case["l_verdict"].as_str(), verdict(&case)), (Some("missing"), "undecided"), "{}{}", ran.out, ran.err);
}

#[test]
fn keep_will_not_empty_a_directory_another_case_or_an_input_is_in() {
    let f = Fakes::new();
    let c = Check::new(&f);
    let bins = |extra: &[&str]| -> Vec<String> {
        let mut a: Vec<String> = ["--axes", "frontend", "--expect", "report-only", "--frontend"].iter().map(|x| s(x)).collect();
        a.push(f.frontend.to_string_lossy().into_owned());
        a.extend(extra.iter().map(|x| s(x)));
        a
    };
    let env: Vec<(&str, &str)> = c.env.iter().map(|(k, v)| (*k, v.as_str())).collect();

    // `a/b.sql` and `a__b.sql` would both run in `keep/a__b.sql`.
    let inputs = f.path().join("in");
    std::fs::create_dir_all(inputs.join("a")).unwrap();
    for p in ["a/b.sql", "a__b.sql"] {
        std::fs::write(inputs.join(p), pair(&NEQ_OK, &[])).unwrap();
    }
    let ran = run(f.path(), &bins(&["--keep", "keep", "in"]), &env);
    assert_eq!(ran.code, 2, "{}{}", ran.out, ran.err);
    assert!(ran.err.contains("in one directory"), "{}", ran.err);

    // `keep/x.sql/x.sql` alone runs in `keep/x.sql`, which holds it.
    let inside = f.path().join("keep/x.sql/x.sql");
    std::fs::create_dir_all(inside.parent().unwrap()).unwrap();
    std::fs::write(&inside, pair(&NEQ_OK, &[])).unwrap();
    let ran = run(f.path(), &bins(&["--keep", "keep", "keep/x.sql/x.sql"]), &env);
    assert_eq!(ran.code, 2, "{}{}", ran.out, ran.err);
    assert!(ran.err.contains("which holds the input"), "{}", ran.err);
    assert!(inside.exists());
}

#[test]
fn a_relative_keep_still_reaches_the_solver_under_portfolio() {
    let f = neq_case();
    let mut c = Check::new(&f);
    c.set("FAKE_SS", "EQ");
    // Relative to the run's working directory, which is not the directory the job is packaged in.
    let (ran, case, _) = c.case(&["--portfolio", "--axes", "frontend,sqleq-solver", "--expect", "report-only", "--keep", "keep"]);
    assert_eq!(case["s_bucket"], "proved", "{}{}\n{}", ran.out, ran.err, case["s_note"]);
    assert_eq!(verdict(&case), "equivalent");
    assert!(f.path().join("keep/case.sql/ss.job.jsonl").exists());
}

#[test]
fn a_packaging_failure_that_is_not_the_bridges_refusal_is_an_error() {
    let f = neq_case();
    let mut c = Check::new(&f);
    let staged = ["--axes", "frontend,sqleq-solver", "--expect", "report-only", "--no-retry"];
    for (label, body, bucket) in [
        ("a plan it cannot read back", "echo 'case.json is not an Input JSON: recursion limit exceeded' >&2; exit 2", "error"),
        ("a job it cannot write", "echo 'cannot write job.jsonl: No such file or directory (os error 2)' >&2; exit 1", "error"),
        ("a crash", "kill -SEGV $$", "error"),
        ("the bridge's refusal", "echo 'two tables share a bare name; a scan index cannot address them' >&2; exit 1", "unsupported"),
    ] {
        let body = format!("case \" $* \" in *\" --sqlsolver \"*) {body} ;; esac");
        c.frontend = frontend(f.path(), "fe-pack", &body, &f);
        let (ran, case, _) = c.case(&staged);
        assert_eq!(case["s_bucket"], bucket, "{label}: {}{}\n{}", ran.out, ran.err, case["s_note"]);
    }
}

// --- 5. A frontend crash ---------------------------------------------------------------------------

#[test]
fn a_frontend_crash_is_an_error_and_not_a_refusal() {
    let f = neq_case();
    let mut c = Check::new(&f);
    let args = ["--axes", "frontend,qed", "--expect", "report-only", "--no-retry"];
    let panic = "printf '%s\\n' \"thread 'main' panicked at src/x.rs:1:1:\" 'index out of bounds' \
                 'note: run with `RUST_BACKTRACE=1` environment variable to display a backtrace' >&2; exit 101";
    for (label, body, says) in [
        ("a SIGSEGV with nothing on stderr", "kill -SEGV $$", "frontend killed by signal 11 without writing a plan"),
        ("a stack overflow", "echo 'fatal runtime error: stack overflow, aborting' >&2; kill -ABRT $$", "frontend killed by signal 6: fatal runtime error: stack overflow, aborting"),
        ("a panic", panic, "frontend exit 101: index out of bounds"),
        ("an exit 1 with no reason", "exit 1", "frontend exit 1 without writing a plan"),
        ("a clean exit with no plan", "exit 0", "frontend exit 0 without writing a plan"),
    ] {
        c.frontend = exe(&f.path().join("fe-crash"), body);
        let (ran, case, _) = c.case(&args);
        assert_eq!(case["status"], "error", "{label}: {}{}", ran.out, ran.err);
        assert_eq!((case["refuse_kind"].as_str(), case["message"].as_str()), (Some(""), Some(says)), "{label}");
    }

    // The control: the frontend's own refusal, exit 1 with its reason, is still one.
    c.frontend = f.frontend.clone();
    c.set("FAKE_FE_STATUS", "unsupported");
    let (_, case, _) = c.case(&args);
    assert_eq!((case["status"].as_str(), case["refuse_kind"].as_str()), (Some("refused"), Some("unsupported")));
}

#[test]
fn a_frontend_crash_is_retried_like_a_prover_crash() {
    let f = neq_case();
    let mut c = Check::new(&f);
    c.set("FAKE_QED", "proved");
    let marker = f.path().join("crashed-once");
    let body = format!("if [ ! -e '{m}' ]; then : > '{m}'; kill -ABRT $$; fi", m = marker.display());
    c.frontend = frontend(f.path(), "fe-once", &body, &f);

    let (ran, case, _) = c.case(&["--axes", "frontend,qed", "--expect", "report-only"]);
    assert!(ran.out.contains("re-running 1 transient failure(s)"), "{}", ran.out);
    assert_eq!(case["status"], "provable", "{}{}", ran.out, ran.err);

    std::fs::remove_file(&marker).unwrap();
    let (ran, case, _) = c.case(&["--portfolio", "--axes", "frontend,qed", "--expect", "report-only"]);
    assert_eq!(verdict(&case), "equivalent", "{}{}", ran.out, ran.err);
    assert_eq!(case["portfolio"]["retried"], true);
}

#[test]
fn bless_will_not_pin_a_frontend_crash_as_a_refusal() {
    let f = Fakes::new();
    let mut c = Check::new(&f);
    let text = pair(&NEQ_OK, &["-- expect frontend: emit"]);
    std::fs::write(&f.case, &text).unwrap();
    c.frontend = exe(&f.path().join("fe-crash"), "echo 'fatal runtime error: stack overflow, aborting' >&2; kill -ABRT $$");
    let ran = c.run(&["--expect", "pinned", "--bless", "--axes", "frontend"]);
    assert_eq!(ran.code, 1, "{}{}", ran.out, ran.err);
    assert_eq!(std::fs::read_to_string(&f.case).unwrap(), text, "a crash is no answer to pin");
}
