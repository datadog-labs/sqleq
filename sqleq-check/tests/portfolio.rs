// Unless explicitly stated otherwise all files in this repository are licensed under the
// Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/).
// Copyright 2026-Present Datadog, Inc.

//! `--portfolio` end to end, over stand-in backends: that they run at once, that the deadline is
//! one deadline and kills what is left, and that the combined verdict decides the exit code.

mod common;

use std::time::Instant;

use common::*;
use serde_json::Value;

struct Portfolio<'a> {
    f: &'a Fakes,
    env: Vec<(&'a str, String)>,
}

impl<'a> Portfolio<'a> {
    fn new(f: &'a Fakes) -> Portfolio<'a> {
        let mut p = Portfolio { f, env: Vec::new() };
        p.set("FAKE_ARGV", &f.argv_log.to_string_lossy());
        p.set("FAKE_QED", "no-proof").set("FAKE_SS", "NEQ").set("FAKE_FUZZ", "NO-COUNTEREXAMPLE");
        p
    }

    fn set(&mut self, k: &'a str, v: &str) -> &mut Self {
        self.env.retain(|(x, _)| *x != k);
        self.env.push((k, v.to_string()));
        self
    }

    fn run(&self, extra: &[&str]) -> Ran {
        let f = self.f;
        let mut argv: Vec<String> = vec![s("--portfolio"), s("-j"), s("1")];
        for (flag, bin) in [
            ("--frontend", &f.frontend),
            ("--prover", &f.prover),
            ("--sqleq-solver-bin", &f.solver),
            ("--fuzz-bin", &f.fuzz),
            ("--lean-bin", &f.lean),
        ] {
            argv.extend([s(flag), bin.to_string_lossy().into_owned()]);
        }
        argv.extend(extra.iter().map(|x| s(x)));
        argv.push(f.case.to_string_lossy().into_owned());
        let env: Vec<(&str, &str)> = self.env.iter().map(|(k, v)| (*k, v.as_str())).collect();
        run(f.path(), &argv, &env)
    }

    /// Run with `--json`, and return the one case's record.
    fn case(&self, extra: &[&str]) -> (Ran, Value) {
        let out = self.f.path().join("out.json");
        let mut a: Vec<&str> = extra.to_vec();
        let o = out.to_string_lossy().into_owned();
        a.extend(["--json", &o]);
        let ran = self.run(&a);
        let v: Value = serde_json::from_str(&std::fs::read_to_string(&out).unwrap_or_default()).unwrap_or(Value::Null);
        let case = v["cases"][0].clone();
        (ran, case)
    }
}

fn verdict(case: &Value) -> &str {
    case["portfolio"]["verdict"].as_str().unwrap_or("-")
}

fn strings(v: &Value) -> Vec<String> {
    v.as_array().into_iter().flatten().filter_map(|x| x.as_str().map(str::to_string)).collect()
}

fn setup() -> Fakes {
    let f = Fakes::new();
    std::fs::write(&f.case, pair(&EQ_OK, &[])).unwrap();
    f
}

#[test]
fn the_backends_run_at_once() {
    let f = setup();
    let mut p = Portfolio::new(&f);
    p.set("FAKE_QED_SLEEP", "1.5").set("FAKE_SS_SLEEP", "1.5").set("FAKE_FUZZ_SLEEP", "1.5");
    let t0 = Instant::now();
    let (ran, case) = p.case(&["--expect", "report-only"]);
    let wall = t0.elapsed().as_secs_f64();
    assert_eq!(ran.code, 0, "{}{}", ran.out, ran.err);
    assert_eq!(verdict(&case), "undecided");
    // Three 1.5s backends one after another would take 4.5s.
    assert!(wall < 3.0, "the backends ran one after another: {wall:.2}s");
    let done = case["portfolio"]["done"].as_object().unwrap();
    for axis in ["frontend", "qed", "sqleq-solver", "fuzz"] {
        assert!(done.contains_key(axis), "{axis} has no answer time: {done:?}");
    }
}

#[test]
fn the_deadline_is_shared_and_kills_what_is_left() {
    let f = setup();
    let pids = f.path().join("pids");
    let mut p = Portfolio::new(&f);
    p.set("FAKE_QED_SLEEP", "30").set("FAKE_SS", "EQ").set("FAKE_PIDS", &pids.to_string_lossy());
    let t0 = Instant::now();
    let (ran, case) = p.case(&["-t", "2", "--no-retry"]);
    assert!(t0.elapsed().as_secs_f64() < 6.0, "the run outlived its deadline");
    // sqleq-solver's proof decides the case even though the prover never answered.
    assert_eq!(ran.code, 0, "{}{}", ran.out, ran.err);
    assert_eq!(verdict(&case), "equivalent");
    assert_eq!(strings(&case["portfolio"]["by"]), ["sqleq-solver"]);
    assert_eq!(strings(&case["portfolio"]["pending"]), ["qed"]);
    assert_eq!(case["status"], "timeout");
    let pid: i32 = std::fs::read_to_string(&pids).unwrap().trim().parse().unwrap();
    std::thread::sleep(std::time::Duration::from_millis(100));
    let alive = std::fs::read_to_string(format!("/proc/{pid}/stat"))
        .map(|s| s.rsplit(')').next().and_then(|t| t.split_whitespace().next()) != Some("Z"))
        .unwrap_or(false);
    assert!(!alive, "the prover ({pid}) outlived the deadline");
}

#[test]
fn nothing_decisive_by_the_deadline_is_a_timeout() {
    let f = setup();
    let mut p = Portfolio::new(&f);
    p.set("FAKE_QED_SLEEP", "30").set("FAKE_SS_SLEEP", "30");
    let (ran, case) = p.case(&["-t", "1", "--no-retry", "--expect", "report-only"]);
    assert_eq!(ran.code, 0);
    assert_eq!(verdict(&case), "timeout");
    assert_eq!(strings(&case["portfolio"]["pending"]), ["qed", "sqleq-solver"]);
    assert!(ran.out.contains("cut off"), "{}", ran.out);
}

#[test]
fn a_proof_and_a_counterexample_are_an_alarm_that_fails_every_policy() {
    let f = setup();
    let mut p = Portfolio::new(&f);
    p.set("FAKE_QED", "proved").set("FAKE_FUZZ", "NOT-EQUIVALENT");
    let (ran, case) = p.case(&["--expect", "report-only"]);
    assert_eq!(verdict(&case), "alarm");
    assert_eq!(ran.code, 1, "an alarm fails even a report-only run");
    assert!(ran.out.contains("ALARM"), "{}", ran.out);
    assert_eq!(p.run(&["--expect", "equivalent"]).code, 1);
}

#[test]
fn a_counterexample_is_not_equivalent() {
    let f = setup();
    let mut p = Portfolio::new(&f);
    p.set("FAKE_FUZZ", "NOT-EQUIVALENT");
    let (ran, case) = p.case(&["--expect", "report-only"]);
    assert_eq!((ran.code, verdict(&case)), (0, "not-equivalent"));
    assert_eq!(strings(&case["portfolio"]["by"]), ["fuzz"]);
    assert_eq!(p.run(&[]).code, 1, "--expect equivalent fails on it");
}

#[test]
fn either_prover_satisfies_expect_equivalent() {
    let f = setup();
    let mut p = Portfolio::new(&f);
    p.set("FAKE_SS", "EQ");
    let (ran, case) = p.case(&[]);
    assert_eq!((ran.code, verdict(&case)), (0, "equivalent"), "{}{}", ran.out, ran.err);
    p.set("FAKE_SS", "NEQ").set("FAKE_QED", "proved");
    assert_eq!(p.run(&[]).code, 0);
    p.set("FAKE_QED", "no-proof");
    assert_eq!(p.run(&[]).code, 1, "no proof, no pass");
}

#[test]
fn a_gather_proof_is_its_own_verdict_and_not_equivalent_enough() {
    let f = setup();
    let mut p = Portfolio::new(&f);
    p.set("FAKE_LEAN", "proved-gather");
    let (ran, case) = p.case(&["--axes", "lean"]);
    assert_eq!(verdict(&case), "equivalent-gather");
    assert_eq!(ran.code, 1, "--expect equivalent is a claim under index binding");
    assert!(!f.argv_log.exists(), "the Lean axis alone needs no frontend");
}

#[test]
fn a_case_left_undecided_by_the_deadline_is_retried_serially() {
    let f = setup();
    let once = f.path().join("slept");
    let mut p = Portfolio::new(&f);
    p.set("FAKE_QED", "proved").set("FAKE_QED_ONCE", &once.to_string_lossy());
    let (ran, case) = p.case(&["-t", "1"]);
    assert_eq!((ran.code, verdict(&case)), (0, "equivalent"), "{}{}", ran.out, ran.err);
    assert_eq!(case["portfolio"]["retried"], true);
    assert!(ran.out.contains("re-running 1 undecided case(s)"), "{}", ran.out);
}

#[test]
fn the_reports_carry_the_verdict() {
    let f = setup();
    let mut p = Portfolio::new(&f);
    p.set("FAKE_SS", "EQ");
    let out = f.path().join("out.json");
    let csv = f.path().join("out.csv");
    let ran = p.run(&["--json", &out.to_string_lossy(), "--csv", &csv.to_string_lossy()]);
    assert_eq!(ran.code, 0);
    let v: Value = serde_json::from_str(&std::fs::read_to_string(&out).unwrap()).unwrap();
    assert_eq!(v["meta"]["portfolio"]["counts"]["equivalent"], 1);
    assert_eq!(strings(&v["meta"]["portfolio"]["backends"]), ["fuzz", "qed", "sqleq-solver"]);
    let text = std::fs::read_to_string(&csv).unwrap();
    let header = text.lines().next().unwrap();
    assert!(header.ends_with("p_verdict,p_by,p_pending,p_first_s,p_retried"), "{header}");
    assert!(text.lines().nth(1).unwrap().contains(",equivalent,sqleq-solver,"), "{text}");
}

#[test]
fn a_run_without_the_portfolio_writes_no_portfolio_fields() {
    let f = setup();
    let p = Portfolio::new(&f);
    let out = f.path().join("plain.json");
    let argv: Vec<String> = [
        "--expect",
        "report-only",
        "--frontend",
        &f.frontend.to_string_lossy(),
        "--prover",
        &f.prover.to_string_lossy(),
        "--json",
        &out.to_string_lossy(),
        &f.case.to_string_lossy(),
    ]
    .iter()
    .map(|x| s(x))
    .collect();
    let env: Vec<(&str, &str)> = p.env.iter().map(|(k, v)| (*k, v.as_str())).collect();
    assert_eq!(run(f.path(), &argv, &env).code, 0);
    let v: Value = serde_json::from_str(&std::fs::read_to_string(&out).unwrap()).unwrap();
    assert!(v["meta"].get("portfolio").is_none() && v["cases"][0].get("portfolio").is_none());
}

#[test]
fn what_a_portfolio_cannot_be_combined_with_is_a_setup_error() {
    let f = setup();
    let p = Portfolio::new(&f);
    for extra in [
        vec!["--expect", "pinned"],
        vec!["--expect", "pinned", "--bless"],
        vec!["--sqlsolver-jvm"],
        vec!["--axes", "frontend"],
    ] {
        let ran = p.run(&extra);
        assert_eq!(ran.code, 2, "{extra:?}: {}{}", ran.out, ran.err);
    }
}
