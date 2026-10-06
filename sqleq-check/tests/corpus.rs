// Unless explicitly stated otherwise all files in this repository are licensed under the
// Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/).
// Copyright 2026-Present Datadog, Inc.

//! `--corpus` and the options a long corpus run needs, over stand-in backends.

mod common;

use std::path::{Path, PathBuf};

use common::*;
use serde_json::Value;

/// Four lines; the second has one field, so it is no pair, but it still uses up row 1.
const CORPUS: &str = "\"SELECT a FROM t\",\"SELECT a FROM t WHERE true\",\"CREATE TABLE t (a INTEGER);\"\n\
                      \"just one field\"\n\
                      \"SELECT $1\",\"SELECT $1\"\n\
                      \"SELECT b FROM u\",\"SELECT b FROM u\",\"CREATE TABLE u (b INTEGER);\"\n";

struct Run<'a> {
    f: &'a Fakes,
    corpus: PathBuf,
    env: Vec<(&'a str, String)>,
}

impl<'a> Run<'a> {
    fn new(f: &'a Fakes) -> Run<'a> {
        let corpus = f.path().join("corpus.csv");
        std::fs::write(&corpus, CORPUS).unwrap();
        let mut r = Run { f, corpus, env: Vec::new() };
        r.set("FAKE_ARGV", &f.argv_log.to_string_lossy());
        r.set("FAKE_QED", "proved").set("FAKE_SS", "NEQ").set("FAKE_FUZZ", "NO-COUNTEREXAMPLE");
        r
    }

    fn set(&mut self, k: &'a str, v: &str) -> &mut Self {
        self.env.retain(|(x, _)| *x != k);
        self.env.push((k, v.to_string()));
        self
    }

    fn bins(&self) -> Vec<String> {
        let f = self.f;
        let mut a = Vec::new();
        for (flag, bin) in [
            ("--frontend", &f.frontend),
            ("--prover", &f.prover),
            ("--sqleq-solver-bin", &f.solver),
            ("--fuzz-bin", &f.fuzz),
            ("--lean-bin", &f.lean),
        ] {
            a.extend([s(flag), bin.to_string_lossy().into_owned()]);
        }
        a
    }

    fn raw(&self, argv: &[String]) -> Ran {
        let env: Vec<(&str, &str)> = self.env.iter().map(|(k, v)| (*k, v.as_str())).collect();
        run(self.f.path(), argv, &env)
    }

    /// Run over the corpus with the stand-ins, `--expect report-only`, and return the cases by name.
    fn cases(&self, extra: &[&str]) -> (Ran, Vec<Value>) {
        let out = self.f.path().join("out.json");
        let _ = std::fs::remove_file(&out);
        let mut argv = vec![s("--corpus"), self.corpus.to_string_lossy().into_owned(), s("--expect"), s("report-only")];
        argv.extend(self.bins());
        argv.extend(extra.iter().map(|x| s(x)));
        argv.extend([s("--json"), out.to_string_lossy().into_owned()]);
        let ran = self.raw(&argv);
        let v: Value = serde_json::from_str(&std::fs::read_to_string(&out).unwrap_or_default()).unwrap_or(Value::Null);
        let mut cases: Vec<Value> = v["cases"].as_array().cloned().unwrap_or_default();
        cases.sort_by_key(|c| c["name"].as_str().unwrap_or("").to_string());
        (ran, cases)
    }
}

/// Row `n`'s case name, as the corpus readers spell it.
fn row(n: usize) -> String {
    format!("pair{n:04}")
}

fn names(cases: &[Value]) -> Vec<&str> {
    cases.iter().map(|c| c["name"].as_str().unwrap_or("")).collect()
}

fn argv_lines(f: &Fakes) -> Vec<String> {
    std::fs::read_to_string(&f.argv_log).unwrap_or_default().lines().map(str::to_string).collect()
}

#[test]
fn rows_are_named_by_their_index_and_reach_every_axis_as_rows() {
    let f = Fakes::new();
    let mut r = Run::new(&f);
    let fuzz_argv = f.path().join("fuzz.argv");
    r.set("FAKE_FUZZ_ARGV", &fuzz_argv.to_string_lossy()).set("FAKE_LEAN", "unsupported");
    let (ran, cases) = r.cases(&["--axes", "frontend,qed,fuzz,lean"]);
    assert_eq!(ran.code, 0, "{}{}", ran.out, ran.err);
    assert_eq!(names(&cases), [row(0), row(2), row(3)], "row 1 is no pair, and still uses up its number");
    for c in &cases {
        assert_eq!(c["status"], "provable", "{c}");
        assert_eq!(c["f_verdict"], "no-counterexample");
        assert_eq!(c["l_raw"]["generated"], true, "the Lean record is kept whole");
    }
    // The frontend saw each row as a one-row corpus, and sqleq-fuzz ran its `row` mode on it.
    assert!(argv_lines(&f).iter().all(|l| l.contains("--csv") && l.contains("--report")), "{:?}", argv_lines(&f));
    let fz = std::fs::read_to_string(&fuzz_argv).unwrap();
    assert_eq!(fz.lines().filter(|l| l.starts_with("row row.csv 0 ")).count(), 3, "{fz}");
}

#[test]
fn only_keeps_the_named_rows_without_renumbering() {
    let f = Fakes::new();
    let r = Run::new(&f);
    let only = f.path().join("only.txt");
    std::fs::write(&only, format!("{}\n\n", row(3))).unwrap();
    let (_, cases) = r.cases(&["--axes", "frontend", "--only", &only.to_string_lossy()]);
    assert_eq!(names(&cases), [row(3)]);
}

#[test]
fn the_catalog_reaches_the_frontend_and_a_header_still_wins() {
    let f = Fakes::new();
    let r = Run::new(&f);
    r.cases(&["--axes", "frontend", "--catalog", "inferred-seeded"]);
    assert!(argv_lines(&f).iter().all(|l| l.split(' ').any(|a| a == "--infer-seeded")), "{:?}", argv_lines(&f));

    // A pair file's own header names what that pair needs, over the run's default.
    let g = Fakes::new();
    std::fs::write(&g.case, pair(&NEQ_OK, &["-- catalog: inferred"])).unwrap();
    let rg = Run::new(&g);
    let mut argv = vec![s("--expect"), s("report-only"), s("--axes"), s("frontend"), s("--catalog"), s("declared")];
    argv.extend(rg.bins());
    argv.push(g.case.to_string_lossy().into_owned());
    assert_eq!(rg.raw(&argv).code, 0);
    let first = argv_lines(&g).into_iter().next().unwrap_or_default();
    assert!(first.split(' ').any(|a| a == "--infer"), "{first}");
}

#[test]
fn a_refused_row_carries_its_reason_and_whether_reflexivity_settled_it() {
    let f = Fakes::new();
    let mut r = Run::new(&f);
    r.set("FAKE_FE_STATUS", "reflexive");
    let (_, cases) = r.cases(&["--axes", "frontend"]);
    let c = &cases[0];
    assert_eq!((c["status"].as_str(), c["refuse_kind"].as_str()), (Some("refused"), Some("unsupported")));
    assert_eq!(c["message"], "unsupported: LIMIT");
    assert_eq!(c["reflexive"], true);
    r.set("FAKE_FE_STATUS", "refuse");
    let (_, cases) = r.cases(&["--axes", "frontend"]);
    assert!(cases[0].get("reflexive").is_none(), "only written when it is true");
}

#[test]
fn the_portfolio_credits_a_reflexive_row() {
    let f = Fakes::new();
    let mut r = Run::new(&f);
    r.set("FAKE_FE_STATUS", "reflexive");
    let (_, cases) = r.cases(&["--portfolio", "--axes", "frontend,qed"]);
    assert_eq!(cases[0]["portfolio"]["verdict"], "equivalent", "{}", cases[0]);
    assert_eq!(cases[0]["portfolio"]["by"], serde_json::json!(["frontend"]));
    r.set("FAKE_FE_STATUS", "refuse");
    let (_, cases) = r.cases(&["--portfolio", "--axes", "frontend,qed"]);
    assert_eq!(cases[0]["portfolio"]["verdict"], "undecided", "{}", cases[0]);
}

#[test]
fn jsonl_streams_every_case_and_resume_skips_the_ones_it_holds() {
    let f = Fakes::new();
    let r = Run::new(&f);
    let jsonl = f.path().join("run.jsonl");
    let j = jsonl.to_string_lossy().into_owned();
    // A torn last line, as a killed run leaves it, is not a case.
    std::fs::write(&jsonl, format!("{{\"name\": \"{}\"}}\n{{\"name\": \"pai", row(0))).unwrap();
    let (ran, cases) = r.cases(&["--axes", "frontend,qed", "--jsonl", &j, "--resume"]);
    assert_eq!(ran.code, 0, "{}{}", ran.out, ran.err);
    assert_eq!(names(&cases), [row(2), row(3)], "row 0 was already there");
    let lines = std::fs::read_to_string(&jsonl).unwrap();
    let written: Vec<Value> = lines.lines().filter_map(|l| serde_json::from_str(l).ok()).collect();
    assert_eq!(written.len(), 3);
    assert_eq!(written[2]["status"], "provable", "each line is the whole case");

    let calls = argv_lines(&f).len();
    let (_, cases) = r.cases(&["--axes", "frontend,qed", "--jsonl", &j, "--resume"]);
    assert!(cases.is_empty() && argv_lines(&f).len() == calls, "nothing left to run");

    // Without --resume the file starts over.
    r.cases(&["--axes", "frontend,qed", "--jsonl", &j]);
    assert_eq!(std::fs::read_to_string(&jsonl).unwrap().lines().count(), 3);
}

#[test]
fn a_case_goes_to_jsonl_only_after_its_last_axis() {
    let f = Fakes::new();
    let r = Run::new(&f);
    let jsonl = f.path().join("run.jsonl");
    r.cases(&["--axes", "frontend,qed,sqleq-solver,fuzz", "--jsonl", &jsonl.to_string_lossy()]);
    let written: Vec<Value> =
        std::fs::read_to_string(&jsonl).unwrap().lines().filter_map(|l| serde_json::from_str(l).ok()).collect();
    assert_eq!(written.len(), 3, "once each");
    for c in &written {
        assert!(c["s_bucket"].is_string() && c["f_verdict"].is_string(), "{c}");
    }
}

#[test]
fn bin_dir_supplies_every_backend_and_a_missing_one_is_an_error() {
    let f = Fakes::new();
    let r = Run::new(&f);
    let dir = f.path().join("bin");
    std::fs::create_dir(&dir).unwrap();
    exe(&dir.join("sqleq-frontend"), FAKE_FRONTEND);
    exe(&dir.join("sqleq-fuzz"), FAKE_FUZZ);
    let corpus = r.corpus.to_string_lossy().into_owned();
    let d = dir.to_string_lossy().into_owned();
    let argv = |axes: &str| -> Vec<String> {
        ["--corpus", &corpus, "--expect", "report-only", "--axes", axes, "--bin-dir", &d].iter().map(|x| s(x)).collect()
    };
    let ran = r.raw(&argv("frontend,fuzz"));
    assert_eq!(ran.code, 0, "{}{}", ran.out, ran.err);
    assert!(ran.out.contains(&format!("{d}/sqleq-frontend")), "{}", ran.out);
    let ran = r.raw(&argv("frontend,sqleq-solver"));
    assert_eq!(ran.code, 2);
    assert!(ran.err.contains("no executable sqleq-solver in --bin-dir"), "{}", ran.err);
}

#[test]
fn the_retry_pass_has_its_own_budget() {
    let f = Fakes::new();
    let mut r = Run::new(&f);
    r.set("FAKE_QED_SLEEP", "2");
    let only = f.path().join("only.txt");
    std::fs::write(&only, format!("{}\n", row(0))).unwrap();
    let o = only.to_string_lossy().into_owned();
    let (_, cases) = r.cases(&["--axes", "frontend,qed", "--only", &o, "-t", "1"]);
    assert_eq!(cases[0]["status"], "timeout", "the first tier's budget is 1s, and so is the retry's");
    let (ran, cases) = r.cases(&["--axes", "frontend,qed", "--only", &o, "-t", "1", "--retry-timeout", "10"]);
    assert_eq!(cases[0]["status"], "provable", "the second tier has 10s: {}", ran.out);
    assert!(ran.out.contains("10s each"), "{}", ran.out);
}

#[test]
fn a_solver_row_the_driver_dies_on_is_an_error_and_the_rest_go_on() {
    let f = Fakes::new();
    let mut r = Run::new(&f);
    r.set("FAKE_SS_DIE_ON", &row(2));
    let (ran, cases) = r.cases(&["--axes", "sqleq-solver"]);
    assert_eq!(ran.code, 0, "{}{}", ran.out, ran.err);
    let by: Vec<(String, &str)> =
        cases.iter().map(|c| (c["name"].as_str().unwrap().to_string(), c["s_bucket"].as_str().unwrap_or("-"))).collect();
    assert_eq!(by, [(row(0), "no-proof"), (row(2), "error"), (row(3), "no-proof")]);
    assert!(cases[1]["s_note"].as_str().unwrap().contains("died on this row"), "{}", cases[1]);
    assert_eq!(cases[1]["s_raw"]["died"], true);
}

#[test]
fn solver_drivers_side_by_side_answer_every_row() {
    let f = Fakes::new();
    let r = Run::new(&f);
    let keep = f.path().join("keep");
    let (ran, cases) = r.cases(&["--axes", "sqleq-solver", "--sqleq-solver-jobs", "2", "--keep", &keep.to_string_lossy()]);
    assert!(ran.out.contains("2 drivers side by side"), "{}", ran.out);
    assert!(cases.iter().all(|c| c["s_bucket"] == "no-proof"), "{cases:?}");
    assert!(keep.join("sqlsolver/results.0.jsonl").exists() && keep.join("sqlsolver/results.1.jsonl").exists());
}

#[test]
fn a_partial_fuzz_run_says_how_many_trials_compared() {
    let f = Fakes::new();
    let mut r = Run::new(&f);
    r.set("FAKE_FUZZ_PARTIAL", "7 trials compared both sides; last error: Binder Error");
    let (_, cases) = r.cases(&["--axes", "fuzz"]);
    let raw = &cases[0]["f_raw"];
    assert_eq!((raw["label"].as_str(), raw["ok_trials"].as_u64()), (Some("NO-COUNTEREXAMPLE"), Some(7)));
    assert_eq!(raw["trial_error"], "Binder Error");
    assert_eq!(cases[0]["f_verdict"], "no-counterexample", "the word is unchanged");
}

#[test]
fn the_prover_runs_under_its_memory_cap() {
    let f = Fakes::new();
    let mut r = Run::new(&f);
    let lim = f.path().join("ulimit");
    r.set("FAKE_ULIMIT", &lim.to_string_lossy());
    r.cases(&["--axes", "frontend,qed", "--qed-mem-gib", "1"]);
    let seen = std::fs::read_to_string(&lim).unwrap();
    assert!(!seen.is_empty() && seen.lines().all(|l| l == "1048576"), "{seen}");
}

#[test]
fn a_crashed_prover_leaves_its_exit_code() {
    let f = Fakes::new();
    let mut r = Run::new(&f);
    r.set("FAKE_QED_SIGNAL", "SEGV");
    let (_, cases) = r.cases(&["--axes", "frontend,qed", "--no-retry"]);
    let c = &cases[0];
    assert_eq!((c["status"].as_str(), c["q_rc"].as_i64()), (Some("panic"), Some(-11)), "{c}");
    assert!(c["q_tail"].as_str().is_some_and(|t| t.contains("--- stderr ---")));
    r.set("FAKE_QED_SIGNAL", "");
    let (_, cases) = r.cases(&["--axes", "frontend,qed"]);
    assert!(cases[0].get("q_rc").is_none(), "only a crash carries one");
}

#[test]
fn what_a_corpus_cannot_be_combined_with_is_a_setup_error() {
    let f = Fakes::new();
    let r = Run::new(&f);
    let corpus = r.corpus.to_string_lossy().into_owned();
    let case = f.case.to_string_lossy().into_owned();
    std::fs::write(&f.case, pair(&NEQ_OK, &[])).unwrap();
    for argv in [
        vec!["--corpus", corpus.as_str(), "--expect", "pinned", "--axes", "frontend"],
        vec!["--corpus", corpus.as_str(), case.as_str()],
        vec!["--resume", case.as_str()],
    ] {
        let mut a: Vec<String> = argv.iter().map(|x| s(x)).collect();
        a.extend(r.bins());
        let ran = r.raw(&a);
        assert_eq!(ran.code, 2, "{argv:?}: {}{}", ran.out, ran.err);
    }
}

#[test]
fn the_real_frontend_lowers_a_row_exactly_as_its_whole_corpus_run_does() {
    let Some(fe) = ["debug", "release"]
        .iter()
        .map(|p| sqleq_check::discover::repo().join("target").join(p).join("sqleq-frontend"))
        .find(|b| sqleq_check::util::is_exe(b))
    else {
        eprintln!("skipped: sqleq-frontend is not built");
        return;
    };
    let corpus = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests").join("data").join("corpus.csv");
    let dir = sqleq_check::util::TempDir::new("sqleq-corpus-test-").unwrap();
    let whole = dir.path().join("whole");
    let report = dir.path().join("whole.json");
    let ran = spawning(|| {
        std::process::Command::new(&fe)
            .args(["--infer-seeded", "--csv"])
            .arg(&corpus)
            .arg("-o")
            .arg(&whole)
            .arg("--report")
            .arg(&report)
            .output()
            .unwrap()
    });
    assert!(ran.status.success(), "{}", String::from_utf8_lossy(&ran.stderr));
    let keep = dir.path().join("keep");
    let jsonl = dir.path().join("rows.jsonl");
    let argv: Vec<String> = [
        "--corpus",
        &corpus.to_string_lossy(),
        "--catalog",
        "inferred-seeded",
        "--axes",
        "frontend",
        "--expect",
        "report-only",
        "--frontend",
        &fe.to_string_lossy(),
        "--keep",
        &keep.to_string_lossy(),
        "--jsonl",
        &jsonl.to_string_lossy(),
    ]
    .iter()
    .map(|x| s(x))
    .collect();
    let ran = run(dir.path(), &argv, &[]);
    assert_eq!(ran.code, 0, "{}{}", ran.out, ran.err);
    let rows: std::collections::HashMap<String, Value> = std::fs::read_to_string(&jsonl)
        .unwrap()
        .lines()
        .map(|l| serde_json::from_str::<Value>(l).unwrap())
        .map(|c| (c["name"].as_str().unwrap().to_string(), c))
        .collect();
    let detail: Value = serde_json::from_str(&std::fs::read_to_string(&report).unwrap()).unwrap();
    let detail = detail["detail"].as_array().unwrap();
    assert!(detail.len() >= 4, "the fixture has an emitted, a refused and a reflexive row at least");
    let mut statuses = std::collections::BTreeSet::new();
    for d in detail {
        let name = d["name"].as_str().unwrap();
        let c = &rows[name];
        let status = if c["lowered"] == true {
            "emit"
        } else if c["reflexive"] == true {
            "reflexive"
        } else {
            "refuse"
        };
        assert_eq!(status, d["status"], "{name}");
        statuses.insert(status);
        if status == "emit" {
            let a = std::fs::read(whole.join(format!("{name}.json"))).unwrap();
            let b = std::fs::read(keep.join(name).join(format!("{name}.json"))).unwrap();
            assert!(a == b, "{name}: the plans differ");
        } else {
            assert_eq!((c["refuse_kind"].as_str(), c["message"].as_str()), (d["kind"].as_str(), d["reason"].as_str()), "{name}");
        }
    }
    assert_eq!(statuses.len(), 3, "the fixture exercises emit, refuse and reflexive: {statuses:?}");
}
