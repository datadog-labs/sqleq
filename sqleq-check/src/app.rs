// Unless explicitly stated otherwise all files in this repository are licensed under the
// Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/).
// Copyright 2026-Present Datadog, Inc.

//! Setup, the passes, and the exit-code policy.

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc;
use std::time::Instant;

use crate::axes::{fuzz, lean, solver};
use crate::case::{run_case, status_rank, Case, Stage, ERROR, LOWERED, PANIC, PROVABLE, TIMEOUT};
use crate::cli::{resolve_axes, Args, Expect};
use crate::discover::{self, SsDriver};
use crate::inputs::{collect_inputs, common_root, display_name, suffix};
use crate::pinned;
use crate::portfolio;
use crate::report::{self, Color, Finding, FuzzMeta, LeanMeta, Meta, PinnedMeta, PortfolioMeta, SsMeta};
use crate::util::round;

/// Everything resolved before a single case runs.
pub struct Env {
    pub axes: Vec<&'static str>,
    pub frontend: Option<String>,
    pub prover: Option<String>,
    pub ss: Option<SsDriver>,
    pub fuzz: Option<String>,
    pub lean: Option<String>,
    pub files: Vec<PathBuf>,
}

/// Every check that can fail before a case runs. An error here is exit code 2: a missing tool is
/// not a failed case.
pub fn setup(args: &Args) -> Result<Env, String> {
    let axes = resolve_axes(args)?;
    let pinned = args.expect == Expect::Pinned;
    if args.portfolio {
        // A pin is an axis's answer, reproducible run to run; a portfolio answer is bought with a
        // time budget shared under load, so pinning one would make the suite flaky.
        if pinned || args.bless {
            return Err("error: --portfolio answers within a time budget, and a pin must not depend on one; \
                        run --expect pinned without --portfolio"
                .into());
        }
        if axes.contains(&"sqlsolver-jvm") {
            return Err("error: --portfolio starts each backend per case, and a JVM's startup would cost more \
                        than the case; ask --sqleq-solver, or the JVM fork without --portfolio"
                .into());
        }
        if !axes.iter().any(|a| ["qed", "sqleq-solver", "fuzz", "lean"].contains(a)) {
            return Err("error: --portfolio needs a backend that can decide: qed, sqleq-solver, fuzz or lean".into());
        }
    }
    if args.bless && !pinned {
        return Err("error: --bless needs --expect pinned".into());
    }
    if args.expect == Expect::Equivalent && !args.portfolio && !axes.contains(&"qed") {
        return Err("error: --expect equivalent is a policy on the qed axis, which --axes leaves out; use \
                    --expect pinned or report-only"
            .into());
    }
    let has = |a: &str| axes.contains(&a);
    let frontend = if has("frontend") { Some(discover::discover_frontend(args.frontend.as_deref())?) } else { None };
    let prover = if has("qed") { Some(discover::discover_prover(args.prover.as_deref())?) } else { None };
    // Resolved before a single case runs -- including the driver rebuild -- so a fork that is
    // missing or will not compile costs a second, not a full pass.
    let ss = if has("sqleq-solver") {
        Some(discover::discover_sqleq_solver(args.sqleq_solver_bin.as_deref())?)
    } else if has("sqlsolver-jvm") {
        Some(discover::discover_sqlsolver_jvm(args.sqlsolver_tree.as_deref())?)
    } else {
        None
    };
    let fuzz = if has("fuzz") { Some(discover::discover_fuzz(args.fuzz_bin.as_deref())?) } else { None };
    let lean = if has("lean") { Some(discover::discover_lean(args.lean_bin.as_deref())?) } else { None };

    let (files, warnings) = collect_inputs(&args.paths);
    for w in warnings {
        eprintln!("{w}");
    }
    if files.is_empty() {
        return Err("error: no .sql or .json inputs found.".into());
    }
    if pinned {
        let plans: Vec<String> =
            files.iter().filter(|f| suffix(f) == ".json").take(3).map(|f| f.to_string_lossy().into_owned()).collect();
        if !plans.is_empty() {
            return Err(format!(
                "error: --expect pinned reads each case's header, and a .json plan has none: {}",
                plans.join(", ")
            ));
        }
    }

    let mut bins: Vec<&str> = [&frontend, &fuzz, &lean].iter().filter_map(|b| b.as_deref()).collect();
    if let Some(d) = ss.as_ref().filter(|d| d.imp == "sqleq-solver") {
        bins.push(&d.location);
    }
    for b in bins {
        if let Some(why) = discover::stale_build(b) {
            if args.bless {
                return Err(format!(
                    "error: {why}; rebuild it before blessing, or the pins record an older tree's answers"
                ));
            }
            eprintln!("warning: {why}; its answers may not be this tree's");
        }
    }
    Ok(Env { axes, frontend, prover, ss, fuzz, lean, files })
}

/// The backends a run asks: every axis but the frontend, which only feeds two of them.
fn backends(axes: &[&'static str]) -> Vec<&'static str> {
    axes.iter().copied().filter(|a| *a != "frontend").collect()
}

fn isatty_stdout() -> bool {
    // SAFETY: isatty(2) on a descriptor this process owns.
    unsafe { libc::isatty(1) == 1 }
}

fn progress(text: &str) {
    print!("{text}\r");
    let _ = std::io::stdout().flush();
}

/// Run every case through `run` on `jobs` workers, calling `done` on each as it finishes.
fn run_all(
    files: &[(PathBuf, String)],
    run: &(dyn Fn(&Path, &str) -> Case + Sync),
    jobs: usize,
    mut done: impl FnMut(Case),
) {
    let next = AtomicUsize::new(0);
    let (tx, rx) = mpsc::channel();
    std::thread::scope(|s| {
        for _ in 0..jobs.max(1).min(files.len().max(1)) {
            let tx = tx.clone();
            let next = &next;
            s.spawn(move || loop {
                let k = next.fetch_add(1, Ordering::Relaxed);
                let Some((f, name)) = files.get(k) else { break };
                if tx.send(run(f, name)).is_err() {
                    break;
                }
            });
        }
        drop(tx);
        for case in rx {
            done(case);
        }
    });
}

pub fn main(args: Args) -> i32 {
    let c = Color { on: !args.no_color && isatty_stdout() };
    let env = match setup(&args) {
        Ok(e) => e,
        Err(msg) => {
            eprintln!("{msg}");
            return 2;
        }
    };
    let axes = env.axes.clone();
    let pinned_mode = args.expect == Expect::Pinned;
    let root = common_root(&env.files);
    let files: Vec<(PathBuf, String)> = env.files.iter().map(|f| (f.clone(), display_name(f, &root))).collect();
    let ss_timeout_ms = args.sqleq_solver_timeout.filter(|t| *t > 0).unwrap_or((args.timeout * 1000.0) as u64);

    let keep_dir = args.keep.as_ref().map(PathBuf::from);
    if let Some(k) = &keep_dir {
        if let Err(e) = std::fs::create_dir_all(k) {
            eprintln!("error: {}: {e}", k.display());
            return 2;
        }
    }

    // One directory for the whole second-opinion pass: a job per case, then the driver's
    // `todo`/`results`. Under --keep it sits beside the kept workdirs, where a refused or
    // surprising row can be replayed by hand.
    let mut ss_tmp: Option<crate::util::TempDir> = None;
    let ss_dir: Option<PathBuf> = match &env.ss {
        // A portfolio packages each case's job in that case's own directory.
        None => None,
        Some(_) if args.portfolio => None,
        Some(_) => {
            let dir = match &keep_dir {
                // Cleared, not reused, unlike the per-case workdirs beside it: the driver resumes
                // from `results.jsonl`, so a previous run's answers left in place would be reported
                // as this run's without a single row being re-asked.
                Some(k) => {
                    let d = k.canonicalize().unwrap_or_else(|_| k.clone()).join("sqlsolver");
                    let _ = std::fs::remove_dir_all(&d);
                    d
                }
                None => match crate::util::TempDir::new("sqleq-ss-") {
                    Ok(t) => {
                        let p = t.path().to_path_buf();
                        ss_tmp = Some(t);
                        p
                    }
                    Err(e) => {
                        eprintln!("error: cannot create a temporary directory: {e}");
                        return 2;
                    }
                },
            };
            if let Err(e) = std::fs::create_dir_all(&dir) {
                eprintln!("error: {}: {e}", dir.display());
                return 2;
            }
            Some(dir)
        }
    };

    if !args.quiet {
        if let Some(f) = &env.frontend {
            println!("{}", c.dim(&format!("sqleq-frontend: {f}")));
        }
        if let Some(p) = &env.prover {
            println!("{}", c.dim(&format!("qed-prover:   {p}")));
        }
        if let Some(d) = &env.ss {
            let label = if d.imp == "sqleq-solver" { "sqleq-solver:" } else { "JVM fork:" };
            println!("{}", c.dim(&format!("{label:<14}{}", d.location)));
        }
        if let Some(f) = &env.fuzz {
            println!("{}", c.dim(&format!("sqleq-fuzz:   {f}")));
        }
        let line = if args.portfolio {
            format!(
                "Checking {} case(s) as a portfolio of {}, all at once within {}s per case, {} case(s) at a time…",
                files.len(),
                backends(&axes).join(", "),
                report::fmt_secs(args.timeout),
                args.jobs
            )
        } else {
            format!(
                "Checking {} case(s) on {} with {} worker(s), {:.0}s/case…",
                files.len(),
                axes.join(", "),
                args.jobs,
                args.timeout
            )
        };
        println!("{}", c.bold(&line));
        println!();
    }

    let name_w = files.iter().map(|(_, n)| n.chars().count()).max().unwrap_or(10).min(60);
    let live = isatty_stdout();
    let mut cases: Vec<Case> = Vec::new();
    let t0 = Instant::now();
    let stage = Stage {
        frontend: env.frontend.clone().unwrap_or_default(),
        prover: env.prover.clone(),
        timeout: args.timeout,
        smt_timeout_ms: args.smt_timeout,
        keep_dir: keep_dir.clone(),
        ss_dir: ss_dir.clone(),
    };
    let pctx = portfolio::Ctx {
        axes: &axes,
        frontend: env.frontend.as_deref(),
        prover: env.prover.as_deref(),
        ss: env.ss.as_ref(),
        ss_cap_ms: args.sqleq_solver_timeout.filter(|t| *t > 0),
        fuzz: env.fuzz.as_deref(),
        lean: env.lean.as_deref(),
        timeout: args.timeout,
        smt_timeout_ms: args.smt_timeout,
        keep_dir: keep_dir.as_deref(),
    };
    let run_one = |f: &Path, name: &str| {
        if args.portfolio {
            portfolio::run_case(f, name, &pctx)
        } else {
            run_case(f, name, &stage)
        }
    };
    if env.frontend.is_none() && !args.portfolio {
        // Only axes that read the pair file themselves: nothing to lower.
        for (f, name) in &files {
            let mut x = Case::new(name, &f.to_string_lossy());
            x.status = LOWERED.into();
            cases.push(x);
        }
    } else {
        let total = files.len();
        run_all(&files, &run_one, args.jobs, |case| {
            let notable = match &case.portfolio {
                Some(o) => o.verdict != portfolio::EQUIVALENT,
                None => case.status != PROVABLE && case.status != LOWERED,
            };
            if !pinned_mode && (args.verbose || (!args.quiet && notable)) {
                report::print_line(c, &case, name_w);
            } else if !args.quiet && live {
                progress(&c.dim(&format!("  [{}/{total}] ", cases.len() + 1)));
            }
            cases.push(case);
        });
    }

    // Retry transient failures serially (no contention) -- a heavy case starved or OOM-killed under
    // -j shouldn't be misreported as a real failure. A refusal is deterministic, so it is never
    // retried.
    let transient = |s: &str| [PANIC, TIMEOUT, ERROR].contains(&s);
    let mut retried = 0;
    if !args.no_retry && args.portfolio {
        // The portfolio's own criterion: a case left without a verdict by the deadline or by a
        // failure that load can cause. The re-run is kept only when it decides the case.
        let retry: Vec<usize> = (0..cases.len()).filter(|&i| portfolio::worth_retrying(&cases[i])).collect();
        if !retry.is_empty() {
            if !args.quiet && live {
                progress(&" ".repeat(30));
            }
            if !args.quiet {
                println!("{}", c.dim(&format!("  re-running {} undecided case(s) serially…", retry.len())));
            }
            for i in retry {
                retried += 1;
                let mut new = portfolio::run_case(Path::new(&cases[i].path), &cases[i].name.clone(), &pctx);
                if let Some(o) = new.portfolio.as_mut() {
                    o.retried = true;
                }
                if new.portfolio.as_ref().is_some_and(|o| portfolio::decisive(&o.verdict)) {
                    if !args.quiet {
                        report::print_line(c, &new, name_w);
                    }
                    cases[i] = new;
                }
            }
        }
    } else if !args.no_retry && env.frontend.is_some() {
        let retry: Vec<usize> = (0..cases.len()).filter(|&i| transient(&cases[i].status)).collect();
        if !retry.is_empty() {
            if !args.quiet && live {
                progress(&" ".repeat(30));
            }
            if !args.quiet {
                println!("{}", c.dim(&format!("  re-running {} transient failure(s) serially…", retry.len())));
            }
            for i in retry {
                let old = &cases[i];
                let new = run_case(Path::new(&old.path), &old.name.clone(), &stage);
                let better = !transient(&new.status) || status_rank(&new.status) < status_rank(&old.status);
                if better {
                    if !args.quiet && !pinned_mode {
                        report::print_case_line(c, &new, name_w);
                    }
                    cases[i] = new;
                }
            }
        }
    }

    let wall = t0.elapsed().as_secs_f64();

    // After the prover pass and its retries, and timed apart from them: the second opinion is a
    // separate question and must not be able to move the numbers above, nor they it.
    let mut ss_stats = solver::Stats::default();
    if args.portfolio {
        // Every axis answered inside the case already; what is left is to count.
        ss_stats.rows = cases.iter().filter(|x| x.s_verdict.is_some() || x.s_bucket.is_some()).count();
        ss_stats.answered = Some(cases.iter().filter(|x| x.s_verdict.is_some()).count());
    } else if let (Some(driver), Some(dir)) = (&env.ss, &ss_dir) {
        if !args.quiet {
            if live {
                progress(&" ".repeat(30));
            }
            let n = cases.iter().filter(|x| x.s_bucket.is_none()).count();
            println!(
                "{}",
                c.dim(&format!(
                    "  asking {} about {n} case(s), sequentially, {ss_timeout_ms}ms/row…",
                    solver::name(&driver.imp)
                ))
            );
        }
        let t1 = Instant::now();
        ss_stats = solver::run_second_opinion(&mut cases, dir, driver, ss_timeout_ms);
        ss_stats.wall_s = Some(round(t1.elapsed().as_secs_f64(), 3));
    }

    // The fuzz axis reads the pair files itself, so it is independent of the passes above --
    // though not of the pair, which is the point.
    let mut fuzz_stats = None;
    let sql_cases = cases.iter().filter(|x| x.is_sql()).count();
    if args.portfolio {
        fuzz_stats = env.fuzz.as_ref().map(|_| fuzz::Stats { rows: sql_cases, wall_s: None });
    } else if let Some(fz) = &env.fuzz {
        if !args.quiet {
            if live {
                progress(&" ".repeat(30));
            }
            let n = cases.iter().filter(|x| x.is_sql()).count();
            println!("{}", c.dim(&format!("  asking sqleq-fuzz about {n} .sql case(s)…")));
        }
        fuzz_stats = Some(fuzz::run_fuzz(&mut cases, fz, args.jobs, args.timeout));
    }

    // The Lean axis, likewise apart from both: it reads the pair files itself.
    let mut lean_stats = None;
    if args.portfolio {
        lean_stats = env.lean.as_ref().map(|_| lean::Stats { rows: sql_cases, ..lean::Stats::default() });
    } else if let Some(lb) = &env.lean {
        if !args.quiet {
            if live {
                progress(&" ".repeat(30));
            }
            let n = cases.iter().filter(|x| x.is_sql()).count();
            println!("{}", c.dim(&format!("  asking sqleq-lean about {n} .sql case(s)…")));
        }
        lean_stats = Some(lean::run_lean(&mut cases, lb, args.jobs, args.timeout, keep_dir.as_deref()));
    }

    if pinned_mode {
        cases.sort_by(|a, b| a.name.cmp(&b.name));
    } else if args.portfolio {
        let rank = |x: &Case| {
            let v = x.portfolio.as_ref().map_or("", |o| o.verdict.as_str());
            portfolio::ORDER.iter().position(|o| *o == v).unwrap_or(99)
        };
        cases.sort_by(|a, b| (rank(a), &a.name).cmp(&(rank(b), &b.name)));
    } else {
        cases.sort_by(|a, b| (status_rank(&a.status), &a.name).cmp(&(status_rank(&b.status), &b.name)));
    }

    if !args.quiet && live {
        progress(&" ".repeat(30));
    }
    let mut pinned = Vec::new();
    let mut blessed = Vec::new();
    if pinned_mode {
        pinned = pinned::judge_cases(&cases, &axes);
        if args.bless {
            blessed = match pinned::bless(&pinned, &cases) {
                Ok(b) => b,
                Err(e) => {
                    eprintln!("error: {e}");
                    return 2;
                }
            };
            // Judged again from the rewritten files, so what is printed and what decides the exit
            // code are the pins as they now stand.
            pinned = pinned::judge_cases(&cases, &axes);
        }
        report::print_pinned(c, &pinned, &cases, &axes);
        if args.bless {
            println!("  {} {} file(s)", c.bold("blessed"), blessed.len());
            for name in &blessed {
                println!("{}", c.dim(&format!("    {name}")));
            }
        }
    } else {
        if env.frontend.is_some() {
            report::print_summary(c, &cases, wall, axes.contains(&"qed"));
        }
        let imp = env.ss.as_ref().map_or("sqleq-solver", |d| d.imp.as_str());
        report::print_second_opinion(c, &cases, &ss_stats, imp);
        if let Some(st) = &fuzz_stats {
            report::print_fuzz(c, &cases, st);
        }
        if let Some(st) = &lean_stats {
            report::print_lean(c, &cases, st);
        }
        if args.portfolio {
            report::print_portfolio(c, &cases, &backends(&axes), args.timeout, retried);
        }
    }

    let meta = Meta {
        axes: axes.iter().map(|a| a.to_string()).collect(),
        frontend: env.frontend.clone(),
        prover: env.prover.clone(),
        jobs: args.jobs,
        timeout_s: args.timeout,
        smt_timeout_ms: args.smt_timeout,
        total: cases.len(),
        wall_s: round(wall, 3),
        triviality: report::triviality_split(&cases),
        sqlsolver: env.ss.as_ref().map(|d| SsMeta {
            stats: ss_stats.clone(),
            imp: d.imp.clone(),
            location: d.location.clone(),
            timeout_ms: ss_timeout_ms,
        }),
        fuzz: env.fuzz.as_ref().map(|b| FuzzMeta {
            stats: fuzz_stats.clone().unwrap_or_default(),
            bin: b.clone(),
            args: fuzz::FUZZ_ARGS.iter().map(|s| s.to_string()).collect(),
        }),
        lean: env.lean.as_ref().map(|b| LeanMeta { stats: lean_stats.clone().unwrap_or_default(), bin: b.clone() }),
        pinned: pinned_mode.then(|| PinnedMeta {
            held: pinned.iter().filter(|p| p.passed()).count(),
            total: pinned.len(),
            blessed: blessed.clone(),
        }),
        portfolio: args.portfolio.then(|| PortfolioMeta::of(&cases, &backends(&axes), args.timeout, retried)),
        findings: pinned_mode.then(|| {
            let mut out = Vec::new();
            for p in &pinned {
                for e in &p.lint {
                    out.push(Finding::Lint { case: cases[p.case].name.clone(), lint: e.clone() });
                }
            }
            for p in &pinned {
                for j in p.judgements.iter().filter(|j| !j.passed()) {
                    out.push(Finding::Moved {
                        case: cases[p.case].name.clone(),
                        axis: j.axis.clone(),
                        state: j.state.to_string(),
                        observed: j.observed.clone(),
                        pinned: j.pin.as_ref().map(|p| p.word.clone()),
                        note: j.note.clone(),
                    });
                }
            }
            out
        }),
    };
    drop(ss_tmp);
    if let Some(path) = &args.json {
        if let Err(e) = report::write_json(path, &cases, &meta) {
            eprintln!("error: {path}: {e}");
            return 2;
        }
        if !args.quiet {
            println!("{}", c.dim(&format!("  wrote {path}")));
        }
    }
    if let Some(path) = &args.csv {
        if let Err(e) = report::write_csv(path, &cases, args.portfolio) {
            eprintln!("error: {path}: {e}");
            return 2;
        }
        if !args.quiet {
            println!("{}", c.dim(&format!("  wrote {path}")));
        }
    }

    let verdict_is = |x: &Case, v: &str| x.portfolio.as_ref().is_some_and(|o| o.verdict == v);
    if args.portfolio {
        // The combined verdict decides, and a soundness alarm fails every policy: it says one of the
        // backends is wrong, which no report should pass over in silence.
        if cases.iter().any(|x| verdict_is(x, portfolio::ALARM)) {
            return 1;
        }
        return match args.expect {
            Expect::Equivalent => i32::from(!cases.iter().all(|x| verdict_is(x, portfolio::EQUIVALENT))),
            _ => 0,
        };
    }
    match args.expect {
        Expect::ReportOnly => 0,
        // Blessing never clears an invariant, a timeout or a lint error, so a bless run that leaves
        // one of those behind still fails.
        Expect::Pinned => i32::from(!pinned.iter().all(|p| p.passed())),
        // Every case must be provable -- by *our* prover. The second opinion is deliberately not
        // part of the policy: adding an axis must not be able to turn a red CI run green, and its
        // `no-proof` is not a failure to begin with.
        Expect::Equivalent => i32::from(!cases.iter().all(|x| x.status == PROVABLE)),
    }
}
