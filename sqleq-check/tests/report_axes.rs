// Unless explicitly stated otherwise all files in this repository are licensed under the
// Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/).
// Copyright 2026-Present Datadog, Inc.

//! The report names only the axes that ran: no comparison with the QED prover when `--axes` leaves
//! it out, a portfolio `proved by` line over the provers asked, and the engine sqleq-fuzz runs on.

mod common;

use common::*;

/// Run the binary on one pair whose two sides lower to different plans (`trivial: false`), with
/// every stand-in backend passed explicitly and `env` set.
fn check(args: &[&str], env: &[(&str, &str)]) -> Ran {
    let f = Fakes::new();
    std::fs::write(&f.case, pair(&EQ_OK, &[])).unwrap();
    let mut argv: Vec<String> = vec![s("-j"), s("1")];
    for (flag, bin) in [
        ("--frontend", &f.frontend),
        ("--prover", &f.prover),
        ("--sqleq-solver-bin", &f.solver),
        ("--fuzz-bin", &f.fuzz),
    ] {
        argv.extend([s(flag), bin.to_string_lossy().into_owned()]);
    }
    argv.extend(args.iter().map(|x| s(x)));
    argv.push(f.case.to_string_lossy().into_owned());
    let log = f.argv_log.to_string_lossy().into_owned();
    let mut all: Vec<(&str, &str)> = vec![("FAKE_ARGV", &log), ("FAKE_FUZZ", "NO-COUNTEREXAMPLE")];
    all.extend_from_slice(env);
    let ran = run(f.path(), &argv, &all);
    assert_eq!(ran.code, 0, "{}{}", ran.out, ran.err);
    ran
}

/// The lines of `out` that start with `prefix`.
fn lines<'a>(out: &'a str, prefix: &str) -> Vec<&'a str> {
    out.lines().filter(|l| l.starts_with(prefix)).collect()
}

/// The starts of the lines that set sqleq-solver against the QED prover.
const COMPARISON: [&str; 6] = [
    "  Second opinion",
    "  of pairs that differ",
    "  both provers",
    "  only the QED prover",
    "  only sqleq-solver",
    "  neither ",
];

fn assert_no_comparison(out: &str) {
    for start in COMPARISON {
        assert!(lines(out, start).is_empty(), "`{start}` without the qed axis:\n{out}");
    }
    assert!(!out.contains("Both opinions"), "the shared-frontend note without the qed axis:\n{out}");
}

#[test]
fn without_qed_the_solver_section_does_not_compare_with_it() {
    let ran = check(&["--expect", "report-only", "--axes", "frontend,sqleq-solver"], &[("FAKE_SS", "EQ")]);
    assert_no_comparison(&ran.out);
    assert_eq!(
        lines(&ran.out, "  SQLSolver axis"),
        ["  SQLSolver axis  — sqleq-solver, over the lowered IR"],
        "{}",
        ran.out
    );
    // The buckets as before, and its own count on the `capability` footing.
    let want = ["  proved                     1", "  proved        1/1  (100.0%)   pairs whose two queries differ"];
    assert_eq!(lines(&ran.out, "  proved "), want, "{}", ran.out);
    assert!(ran.out.contains("`no-proof` is not a refutation"), "{}", ran.out);
}

#[test]
fn without_qed_a_solver_that_proves_nothing_says_so() {
    let ran = check(&["--expect", "report-only", "--axes", "frontend,sqleq-solver"], &[("FAKE_SS", "NEQ")]);
    assert_eq!(
        lines(&ran.out, "  proved "),
        ["  proved        0/1  (0.0%)   pairs whose two queries differ"],
        "{}",
        ran.out
    );
    assert_eq!(lines(&ran.out, "  no-proof "), ["  no-proof                   1"], "{}", ran.out);
}

/// The control: with the QED prover asked, the comparison is printed as it always was.
#[test]
fn with_qed_the_comparison_table_stays() {
    let ran = check(
        &["--expect", "report-only", "--axes", "frontend,qed,sqleq-solver"],
        &[("FAKE_SS", "EQ"), ("FAKE_QED", "proved")],
    );
    let want = [
        "  Second opinion  — sqleq-solver, over the same lowered IR",
        "  of pairs that differ       1",
        "  both provers               1",
        "  only the QED prover        0",
        "  only sqleq-solver          0   what the second opinion adds",
        "  neither                    0",
        "        Both opinions come through this repo's frontend, so where they",
    ];
    for line in want {
        assert_eq!(lines(&ran.out, line), [line], "{}", ran.out);
    }
    assert!(!ran.out.contains("SQLSolver axis"), "{}", ran.out);
    // The summary's `proved n/N` is the qed axis's; the table adds no second one.
    let ratios: Vec<&str> = lines(&ran.out, "  proved ").into_iter().filter(|l| l.contains('/')).collect();
    assert_eq!(ratios, ["  proved        1/1  (100.0%)"], "{}", ran.out);
}

#[test]
fn a_portfolio_without_qed_credits_only_sqleq_solver() {
    let ran =
        check(&["--portfolio", "--expect", "report-only", "--axes", "frontend,sqleq-solver"], &[("FAKE_SS", "EQ")]);
    assert_eq!(lines(&ran.out, "  proved by"), ["  proved by     sqleq-solver 1"], "{}", ran.out);
    assert!(!ran.out.contains("qed alone"), "{}", ran.out);
    assert_no_comparison(&ran.out);
}

#[test]
fn a_portfolio_without_sqleq_solver_credits_only_qed() {
    let ran = check(&["--portfolio", "--expect", "report-only", "--axes", "frontend,qed"], &[("FAKE_QED", "proved")]);
    assert_eq!(lines(&ran.out, "  proved by"), ["  proved by     qed 1"], "{}", ran.out);
    assert!(!ran.out.contains("sqleq-solver alone"), "{}", ran.out);
}

/// The control: with both provers asked, the three-way split is unchanged.
#[test]
fn a_portfolio_of_both_provers_splits_the_proofs_three_ways() {
    let ran = check(
        &["--portfolio", "--expect", "report-only", "--axes", "frontend,qed,sqleq-solver"],
        &[("FAKE_SS", "EQ"), ("FAKE_QED", "proved")],
    );
    assert_eq!(
        lines(&ran.out, "  proved by"),
        ["  proved by     qed alone 0 · sqleq-solver alone 0 · both 1"],
        "{}",
        ran.out
    );
}

#[test]
fn the_fuzz_section_names_the_engine_it_runs_on() {
    let ran = check(&["--expect", "report-only", "--axes", "fuzz"], &[]);
    assert_eq!(
        lines(&ran.out, "  Fuzz axis"),
        ["  Fuzz axis  — sqleq-fuzz, random instances in PostgreSQL"],
        "{}",
        ran.out
    );
    assert!(!ran.out.contains("DuckDB"), "{}", ran.out);
}
