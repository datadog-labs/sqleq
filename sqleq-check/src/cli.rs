// Unless explicitly stated otherwise all files in this repository are licensed under the
// Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/).
// Copyright 2026-Present Datadog, Inc.

//! The command line.

use clap::{Parser, ValueEnum};

use crate::suite;

const EPILOG: &str = "\
Each .sql file holds optional table/function declarations and exactly two statements to compare:
two SELECTs, or two DELETEs, UPDATEs or INSERTs. A .json input is an already-lowered plan, and
--corpus reads the rows of a CSV instead of files.

Exit codes:
  0    policy satisfied (see --expect)
  1    policy not satisfied (some case failed expectation); under --portfolio, any alarm
  2    usage / setup error (a missing tool, a bad flag)
  130  interrupted by Ctrl-C (143 by SIGTERM); every backend still running is killed first";

/// The catalog a case is lowered against when its own header names none.
#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub enum Catalog {
    /// The DDL's tables and columns only; a bare `$N` is refused.
    Declared,
    /// Everything inferred from the queries.
    Inferred,
    /// Tables and columns from the DDL, parameter types inferred.
    InferredSeeded,
}

impl Catalog {
    /// The name a `-- catalog:` header gives it.
    pub fn name(self) -> &'static str {
        match self {
            Catalog::Declared => "declared",
            Catalog::Inferred => "inferred",
            Catalog::InferredSeeded => "inferred-seeded",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub enum Expect {
    /// Nonzero exit unless every case is provable -- for validating known-equivalent rewrite pairs
    /// in CI. Under --portfolio, unless every case's verdict is equivalent.
    Equivalent,
    /// Exit 0 whatever the cases say; under --portfolio an alarm still exits 1.
    ReportOnly,
    /// Every case's header pins each axis's answer, and any movement fails (tests/pairs/README.md).
    Pinned,
}

fn default_jobs() -> usize {
    std::thread::available_parallelism().map_or(4, |n| n.get()).min(8)
}

#[derive(Debug, Parser)]
#[command(
    name = "sqleq-check",
    about = "Batch SQL equivalence checking: run each pair past the axes asked for -- by default \
             the frontend and the QED prover -- and report or judge the answers.",
    after_help = EPILOG,
    infer_long_args = true
)]
pub struct Args {
    /// One or more .sql / .json files or directories (recursed). .json inputs are treated as
    /// pre-parsed plans and skip the frontend stage. A .sql and its sibling .json are
    /// de-duplicated.
    #[arg(value_name = "PATH", required_unless_present = "corpus", conflicts_with = "corpus")]
    pub paths: Vec<String>,

    /// A corpus CSV instead of PATHs: rows of `query A, query B, DDL`, no header line. Row N is case
    /// `pairNNNN`, named and read exactly as the frontend's, sqleq-fuzz's and sqleq-lean's corpus
    /// modes name and read it, and each row reaches every backend through that backend's own corpus
    /// code -- its DDL included, which the corpus reader takes leniently, statement by statement.
    #[arg(long, value_name = "FILE")]
    pub corpus: Option<String>,

    /// Run only the cases named in FILE, one name per line (corpus rows by `pairNNNN`, files by
    /// their display name). Applied after naming, so names never renumber.
    #[arg(long, value_name = "FILE")]
    pub only: Option<String>,

    /// The catalog every case is lowered against unless its own `-- catalog:` header names one
    /// (default: declared).
    #[arg(long, value_enum)]
    pub catalog: Option<Catalog>,

    /// Write each case to FILE as one JSON line the moment its last asked axis has answered: the
    /// same object `--json` holds, written as the run goes, so an interrupted run keeps what it had.
    /// FILE is started afresh unless --resume.
    #[arg(long, value_name = "FILE")]
    pub jsonl: Option<String>,

    /// With --jsonl: keep FILE, skip every case already in it, and append the rest.
    #[arg(long, requires = "jsonl")]
    pub resume: bool,

    /// Cap the QED prover's address space -- its z3 and cvc5 included -- at GIB, as `ulimit -v` does.
    #[arg(long, value_name = "GIB")]
    pub qed_mem_gib: Option<f64>,

    /// Cap each sqleq-solver driver's address space at GIB. A row it dies on is recorded as an
    /// error and the rest go on.
    #[arg(long, value_name = "GIB")]
    pub sqleq_solver_mem_gib: Option<f64>,

    /// The retry pass's own wall-clock budget per case, in seconds (default: --timeout): a second,
    /// longer tier for the cases the first one ran out of time or crashed on.
    #[arg(long, value_name = "S")]
    pub retry_timeout: Option<f64>,

    /// The retry pass's QED_SMT_TIMEOUT, in ms (default: --smt-timeout).
    #[arg(long, value_name = "MS")]
    pub retry_smt_timeout: Option<u64>,

    /// Cases the retry pass runs at once (default: 1, serially, so a case starved under -j gets the
    /// machine to itself).
    #[arg(long, value_name = "N", default_value_t = 1)]
    pub retry_jobs: usize,

    /// sqleq-solver drivers to run side by side, each over its share of the rows (default: 1). More
    /// is faster, but the per-row cap is load-sensitive: a row near it can decide differently with
    /// other drivers running, so one is the reproducible choice. Not used under --portfolio, which
    /// runs sqleq-solver per case.
    #[arg(long, value_name = "N", default_value_t = 1)]
    pub sqleq_solver_jobs: usize,

    /// Take sqleq-frontend, sqleq-fuzz, sqleq-solver and sqleq-lean out of DIR -- every one of them,
    /// unless a flag names it -- instead of looking them up one by one.
    #[arg(long, value_name = "DIR")]
    pub bin_dir: Option<String>,

    /// With the Lean axis: have sqleq-lean also write FILE, what `tools/lean_replay.py` needs to
    /// re-run its proved pairs on Postgres. Not under --portfolio, which runs Lean per case.
    #[arg(long, value_name = "FILE")]
    pub lean_replay_plan: Option<String>,

    /// Parallel cases (default: min(8, ncpu)). Each case itself runs z3+cvc5, so avoid heavy
    /// oversubscription.
    #[arg(short = 'j', long, default_value_t = default_jobs())]
    pub jobs: usize,

    /// Per-case wall-clock timeout in seconds. Under --portfolio, the one deadline every backend
    /// on the case shares.
    #[arg(short = 't', long, default_value_t = 60.0)]
    pub timeout: f64,

    /// Run every asked backend on each case at once, within the one --timeout, and report one
    /// combined verdict per case: equivalent, equivalent-gather or equivalent-gather-generated
    /// (Lean's weaker claims), not-equivalent, alarm (a proof and a counterexample), timeout or
    /// undecided. Asks frontend, qed, sqleq-solver and fuzz unless --axes says otherwise; --lean
    /// adds Lean. The verdict decides the exit code, and an alarm always fails the run. Not with
    /// --expect pinned or --sqlsolver-jvm.
    #[arg(long)]
    pub portfolio: bool,

    /// QED_SMT_TIMEOUT for each SMT request, in ms (default: prover's own default of 10000).
    #[arg(long, value_name = "MS")]
    pub smt_timeout: Option<u64>,

    /// Exit-code policy.
    #[arg(long, value_enum, default_value_t = Expect::Equivalent)]
    pub expect: Expect,

    /// Comma-separated axes to run: frontend, fuzz, qed, sqleq-solver, sqlsolver-jvm, lean
    /// (default: frontend,qed). A prover axis brings in frontend; at most one SQLSolver per run.
    /// sqlsolver-rust is the old name of sqleq-solver.
    #[arg(long, value_name = "LIST")]
    pub axes: Option<String>,

    /// With --expect pinned: rewrite each case's `expect` lines for the axes that ran. Never pins
    /// an answer that contradicts the case's truth, nor a timeout.
    #[arg(long)]
    pub bless: bool,

    /// Path to sqleq-fuzz (else --bin-dir / $SQLEQ_FUZZ / PATH / the newer of this repo's
    /// target/{release,debug} builds).
    #[arg(long, value_name = "PATH")]
    pub fuzz_bin: Option<String>,

    /// Write full results as JSON.
    #[arg(long, value_name = "FILE")]
    pub json: Option<String>,

    /// Write results as CSV.
    #[arg(long, value_name = "FILE")]
    pub csv: Option<String>,

    /// Keep intermediate .json/.result under DIR (default: ephemeral temp dirs, cleaned up).
    #[arg(long, value_name = "DIR")]
    pub keep: Option<String>,

    /// Don't re-run transient failures (panic/timeout/error) serially at the end. By default they
    /// are retried once with no contention, so a heavy case starved under -j isn't misreported as
    /// a failure.
    #[arg(long)]
    pub no_retry: bool,

    /// Print each case as it finishes (not under --expect pinned, which prints a table of pins).
    #[arg(short, long)]
    pub verbose: bool,

    /// Only print the final summary.
    #[arg(short, long)]
    pub quiet: bool,

    /// Disable ANSI color.
    #[arg(long)]
    pub no_color: bool,

    /// Path to sqleq-frontend (else --bin-dir / $SQLEQ_FRONTEND / PATH / the newer of this repo's
    /// target/{release,debug} builds).
    #[arg(long, value_name = "PATH")]
    pub frontend: Option<String>,

    /// Path to qed-prover (else $QED_PROVER / PATH / the newest Nix-wrapped build).
    #[arg(long, value_name = "PATH")]
    pub prover: Option<String>,

    /// Also ask sqleq-solver (a Rust rewrite of SQLSolver) about every case that lowered, over the
    /// same Input JSON the QED prover gets. Outside --portfolio and --expect pinned it never
    /// changes the exit code, and its NEQ is never a refutation. See docs/SQLSOLVER.md.
    #[arg(long, conflicts_with = "sqlsolver_jvm")]
    pub sqleq_solver: bool,

    /// The sqleq-solver binary, whenever a run asks it (else --bin-dir / $SQLEQ_SOLVER_BIN / PATH /
    /// the newer of this repo's target/{release,debug} builds).
    #[arg(long, value_name = "PATH")]
    pub sqleq_solver_bin: Option<String>,

    /// Ask the original SQLSolver instead, as a JVM fork through tools/sqlsolver/IrDriver:
    /// sqleq-solver's backup cross-check, and deprecated. Same jobs, same result rows, same
    /// buckets; needs a JDK and the fork tree. Not with --sqleq-solver or --portfolio.
    #[arg(long)]
    pub sqlsolver_jvm: bool,

    /// With --sqlsolver-jvm: the SQLSolver fork to run (else $SQLEQ_SQLSOLVER; one of the two is
    /// required). Its exploded dependency directory comes from $SQLEQ_SQLSOLVER_DEPS.
    #[arg(long, value_name = "DIR")]
    pub sqlsolver_tree: Option<String>,

    /// Also run the Lean axis (sqleq-lean) over the .sql cases and corpus rows: INSERT ... VALUES
    /// vs INSERT ... SELECT * FROM unnest(..) pairs, proved under the gather rule. The same as
    /// adding `lean` to --axes; outside --expect pinned it never changes the exit code, and under
    /// --portfolio its proof is a gather verdict, not `equivalent`.
    #[arg(long)]
    pub lean: bool,

    /// Path to sqleq-lean (else --bin-dir / $SQLEQ_LEAN / this repo's target/release, then
    /// target/debug). Given but not an executable, it falls through to $SQLEQ_LEAN and the builds,
    /// skipping --bin-dir.
    #[arg(long, value_name = "PATH")]
    pub lean_bin: Option<String>,

    /// Per-row cap for the second opinion (sqleq-solver, or the fork with --sqlsolver-jvm), in ms
    /// (default: --timeout): a budget for each row, not a share of one. Under --portfolio it is
    /// also capped by what is left of the case's deadline.
    #[arg(long, value_name = "MS")]
    pub sqleq_solver_timeout: Option<u64>,
}

/// Which prover gives the second opinion: `sqleq-solver`, `jvm` for the JVM fork, or `None`.
pub fn second_opinion(args: &Args) -> Option<&'static str> {
    if args.sqlsolver_jvm {
        Some("jvm")
    } else if args.sqleq_solver {
        Some("sqleq-solver")
    } else {
        None
    }
}

/// The axes this run asks, in canonical order. `--sqleq-solver` and `--sqlsolver-jvm` add the
/// SQLSolver axis they name, and `sqlsolver-rust`, sqleq-solver's axis before it was renamed, still
/// names it. A prover is handed the frontend's plan, so asking one asks the frontend.
pub fn resolve_axes(args: &Args) -> Result<Vec<&'static str>, String> {
    let mut axes: Vec<String> = match &args.axes {
        None if args.portfolio => ["frontend", "qed", "sqleq-solver", "fuzz"].map(String::from).to_vec(),
        None => vec!["frontend".into(), "qed".into()],
        Some(list) => {
            let named: Vec<String> = list
                .split(',')
                .map(str::trim)
                .filter(|a| !a.is_empty())
                .map(|a| suite::canonical_axis(a).to_string())
                .collect();
            let mut unknown: Vec<&String> = named.iter().filter(|a| !suite::AXES.contains(&a.as_str())).collect();
            unknown.sort();
            unknown.dedup();
            if !unknown.is_empty() {
                let unknown: Vec<&str> = unknown.iter().map(|s| s.as_str()).collect();
                return Err(format!(
                    "error: unknown axis {} (one of {})",
                    unknown.join(", "),
                    suite::AXES.join(", ")
                ));
            }
            named
        }
    };
    if let Some(second) = second_opinion(args) {
        axes.push(if second == "sqleq-solver" { "sqleq-solver".into() } else { "sqlsolver-jvm".into() });
    }
    if args.lean {
        axes.push("lean".into());
    }
    if axes.iter().any(|a| suite::PROVERS.contains(&a.as_str())) {
        axes.push("frontend".into());
    }
    let has = |a: &str| axes.iter().any(|x| x == a);
    if has("sqleq-solver") && has("sqlsolver-jvm") {
        return Err("error: one SQLSolver per run; --bless only touches the axes that ran, so two runs combine."
            .to_string());
    }
    if axes.is_empty() {
        return Err("error: --axes names no axis".to_string());
    }
    Ok(suite::AXES.iter().copied().filter(|a| has(a)).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(argv: &[&str]) -> Result<Args, clap::Error> {
        Args::try_parse_from(["sqleq-check", "x.sql"].iter().chain(argv))
    }

    fn asks(argv: &[&str]) -> Option<&'static str> {
        second_opinion(&parse(argv).unwrap())
    }

    #[test]
    fn nothing_is_asked_by_default() {
        assert_eq!(asks(&[]), None);
    }

    #[test]
    fn each_switch_asks_its_prover() {
        assert_eq!(asks(&["--sqleq-solver"]), Some("sqleq-solver"));
        assert_eq!(asks(&["--sqlsolver-jvm"]), Some("jvm"));
    }

    #[test]
    fn the_two_switches_together_are_a_usage_error() {
        let e = parse(&["--sqleq-solver", "--sqlsolver-jvm"]).unwrap_err();
        assert_eq!(e.exit_code(), 2);
    }

    #[test]
    fn the_switches_and_the_old_axis_name_choose_the_axis() {
        let axes = |argv: &[&str]| resolve_axes(&parse(argv).unwrap());
        assert_eq!(axes(&["--sqleq-solver"]).unwrap(), ["frontend", "qed", "sqleq-solver"]);
        assert_eq!(axes(&["--sqlsolver-jvm"]).unwrap(), ["frontend", "qed", "sqlsolver-jvm"]);
        assert_eq!(axes(&["--axes", "sqlsolver-rust"]).unwrap(), ["frontend", "sqleq-solver"]);
        assert!(axes(&["--axes", "sqleq-solver", "--sqlsolver-jvm"]).is_err());
        assert!(axes(&["--axes", "frontend,qd"]).unwrap_err().contains("unknown axis qd"));
        assert_eq!(axes(&["--axes", "fuzz", "--lean"]).unwrap(), ["fuzz", "lean"]);
    }

    #[test]
    fn a_portfolio_asks_every_light_backend_by_default() {
        let axes = |argv: &[&str]| resolve_axes(&parse(argv).unwrap()).unwrap();
        assert_eq!(axes(&["--portfolio"]), ["frontend", "fuzz", "qed", "sqleq-solver"]);
        assert_eq!(axes(&["--portfolio", "--lean"]), ["frontend", "fuzz", "qed", "sqleq-solver", "lean"]);
        assert_eq!(axes(&["--portfolio", "--axes", "qed,fuzz"]), ["frontend", "fuzz", "qed"]);
    }

    #[test]
    fn long_options_may_be_abbreviated_while_unambiguous() {
        assert!(parse(&["--no-ret"]).unwrap().no_retry);
        assert_eq!(parse(&["--exp", "report-only"]).unwrap().expect, Expect::ReportOnly);
        assert!(parse(&["--sqleq-solver"]).unwrap().sqleq_solver, "an exact name wins over its longer siblings");
    }

    #[test]
    fn values_are_typed() {
        assert_eq!(parse(&["-j3", "-t", "2.5"]).map(|a| (a.jobs, a.timeout)).unwrap(), (3, 2.5));
        assert_eq!(parse(&["--jobs", "x"]).unwrap_err().exit_code(), 2);
        assert_eq!(parse(&["--expect", "maybe"]).unwrap_err().exit_code(), 2);
    }
}
