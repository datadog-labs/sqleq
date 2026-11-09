// Unless explicitly stated otherwise all files in this repository are licensed under the
// Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/).
// Copyright 2026-Present Datadog, Inc.

//! The report names only the axes that ran: sqleq-solver as the run's prover, the QED prover as
//! a second opinion only when asked (and as the prover when `--axes` leaves sqleq-solver out), a
//! portfolio `proved by` line over the provers asked, and the engine sqleq-fuzz runs on.

mod common;

use common::*;

/// Run the binary on one pair whose two sides lower to different plans (`trivial: false`), with
/// every stand-in backend passed explicitly and `env` set; the run must pass.
fn check(args: &[&str], env: &[(&str, &str)]) -> Ran {
    let ran = run_check(args, env);
    assert_eq!(ran.code, 0, "{}{}", ran.out, ran.err);
    ran
}

/// [`check`], whatever the exit code.
fn run_check(args: &[&str], env: &[(&str, &str)]) -> Ran {
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
    run(f.path(), &argv, &all)
}

/// The lines of `out` that start with `prefix`.
fn lines<'a>(out: &'a str, prefix: &str) -> Vec<&'a str> {
    out.lines().filter(|l| l.starts_with(prefix)).collect()
}

/// The starts of the lines that set the two provers against each other.
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
fn by_default_sqleq_solver_is_the_prover_and_nothing_is_compared() {
    let ran = check(&["--expect", "report-only"], &[("FAKE_SS", "EQ")]);
    assert_no_comparison(&ran.out);
    assert_eq!(lines(&ran.out, "  Prover"), ["  Prover  — sqleq-solver, over the lowered IR"], "{}", ran.out);
    // Its buckets, its proved line, and its `capability` on the pairs that differ.
    assert_eq!(lines(&ran.out, "  proved "), ["  proved                     1", "  proved        1/1  (100.0%)"], "{}", ran.out);
    assert_eq!(
        lines(&ran.out, "  capability"),
        ["  capability    1/1  (100.0%)   pairs whose two queries differ"],
        "{}",
        ran.out
    );
    assert!(ran.out.contains("`no-proof` is not a refutation"), "{}", ran.out);
    // The QED prover was not asked, so not looked for: the run passes without it.
    assert!(!ran.out.contains("qed-prover"), "{}", ran.out);
}

#[test]
fn a_prover_that_proves_nothing_says_so() {
    let ran = check(&["--expect", "report-only", "--axes", "frontend,sqleq-solver"], &[("FAKE_SS", "NEQ")]);
    assert_eq!(lines(&ran.out, "  proved "), ["  proved        0/1  (0.0%)"], "{}", ran.out);
    assert_eq!(
        lines(&ran.out, "  capability"),
        ["  capability    0/1  (0.0%)   pairs whose two queries differ"],
        "{}",
        ran.out
    );
    assert_eq!(lines(&ran.out, "  no-proof "), ["  no-proof                   1"], "{}", ran.out);
}

/// With `--qed`, the QED prover is the second opinion, set against sqleq-solver's proofs.
#[test]
fn with_qed_asked_it_is_the_second_opinion() {
    let ran = check(&["--expect", "report-only", "--qed"], &[("FAKE_SS", "EQ"), ("FAKE_QED", "proved")]);
    let want = [
        "  Prover  — sqleq-solver, over the lowered IR",
        "  Second opinion  — the QED prover, over the same lowered IR",
        "  of pairs that differ       1",
        "  both provers               1",
        "  only sqleq-solver          0",
        "  only the QED prover        0   what the second opinion adds",
        "  neither                    0",
        "        Both opinions come through this repo's frontend, so where they",
    ];
    for line in want {
        assert_eq!(lines(&ran.out, line), [line], "{}", ran.out);
    }
    // The one `proved n/N` is sqleq-solver's; the summary prints none for the second opinion.
    let ratios: Vec<&str> = lines(&ran.out, "  proved ").into_iter().filter(|l| l.contains('/')).collect();
    assert_eq!(ratios, ["  proved        1/1  (100.0%)"], "{}", ran.out);
}

/// The control: with `--axes` leaving sqleq-solver out, the QED prover is the run's prover, and the
/// summary carries its proved line, as it did before sqleq-solver was the default.
#[test]
fn without_sqleq_solver_the_qed_prover_is_the_prover() {
    let ran = check(&["--expect", "report-only", "--axes", "frontend,qed"], &[("FAKE_QED", "proved")]);
    assert_no_comparison(&ran.out);
    assert!(!ran.out.contains("  Prover"), "{}", ran.out);
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

/// The default policy reads sqleq-solver, and needs no QED prover: a proved pair exits 0, a
/// `no-proof` exits 1.
#[test]
fn the_default_policy_reads_sqleq_solver() {
    assert_eq!(run_check(&[], &[("FAKE_SS", "EQ")]).code, 0);
    assert_eq!(run_check(&[], &[("FAKE_SS", "NEQ")]).code, 1);
}

/// Asking the QED prover never changes the exit code: its proof cannot pass a pair sqleq-solver did
/// not prove, nor its `unprovable` fail one sqleq-solver did. Adding an axis must not be able to turn
/// a red run green.
#[test]
fn asking_qed_never_changes_the_exit_code() {
    assert_eq!(run_check(&["--qed"], &[("FAKE_SS", "NEQ"), ("FAKE_QED", "proved")]).code, 1);
    assert_eq!(run_check(&["--qed"], &[("FAKE_SS", "EQ")]).code, 0);
}

/// With `--axes` leaving sqleq-solver out, the QED prover decides, as it did before sqleq-solver was
/// the default.
#[test]
fn without_sqleq_solver_the_policy_reads_qed() {
    assert_eq!(run_check(&["--axes", "frontend,qed"], &[("FAKE_QED", "proved")]).code, 0);
    assert_eq!(run_check(&["--axes", "frontend,qed"], &[]).code, 1);
}
