// Unless explicitly stated otherwise all files in this repository are licensed under the
// Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/).
// Copyright 2026-Present Datadog, Inc.

//! Setup, the passes, and the exit-code policy.

use std::collections::HashSet;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{mpsc, Mutex};
use std::time::Instant;

use crate::axes::{fuzz, lean, solver};
use crate::case::{run_case, status_rank, Case, Stage, ERROR, LOWERED, PANIC, PROVABLE, TIMEOUT};
use crate::cli::{resolve_axes, Args, Expect};
use crate::discover::{self, SsDriver};
use crate::inputs::{collect_inputs, common_root, corpus_items, display_name, read_names, suffix, Item};
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
    pub items: Vec<Item>,
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
    if pinned && args.corpus.is_some() {
        return Err("error: --expect pinned reads each case's header, and a corpus row has none; pin pair files".into());
    }
    if args.expect == Expect::Equivalent && !args.portfolio && !axes.contains(&"qed") {
        return Err("error: --expect equivalent is a policy on the qed axis, which --axes leaves out; use \
                    --expect pinned or report-only"
            .into());
    }
    let has = |a: &str| axes.contains(&a);
    let bin_dir = args.bin_dir.as_deref();
    let frontend =
        if has("frontend") { Some(discover::discover_frontend(args.frontend.as_deref(), bin_dir)?) } else { None };
    let prover = if has("qed") { Some(discover::discover_prover(args.prover.as_deref())?) } else { None };
    // Resolved before a single case runs -- including the driver rebuild -- so a fork that is
    // missing or will not compile costs a second, not a full pass.
    let ss = if has("sqleq-solver") {
        Some(discover::discover_sqleq_solver(args.sqleq_solver_bin.as_deref(), bin_dir)?)
    } else if has("sqlsolver-jvm") {
        Some(discover::discover_sqlsolver_jvm(args.sqlsolver_tree.as_deref())?)
    } else {
        None
    };
    let fuzz = if has("fuzz") { Some(discover::discover_fuzz(args.fuzz_bin.as_deref(), bin_dir)?) } else { None };
    let lean = if has("lean") { Some(discover::discover_lean(args.lean_bin.as_deref(), bin_dir)?) } else { None };

    let only = match &args.only {
        Some(p) => Some(read_names(Path::new(p)).map_err(|e| format!("error: --only {e}"))?),
        None => None,
    };
    let items: Vec<Item> = match &args.corpus {
        Some(c) => {
            let items = corpus_items(Path::new(c), only.as_ref()).map_err(|e| format!("error: {e}"))?;
            if items.is_empty() {
                return Err(format!("error: no rows of {c} to run"));
            }
            items
        }
        None => {
            let (files, warnings) = collect_inputs(&args.paths);
            for w in warnings {
                eprintln!("{w}");
            }
            if files.is_empty() {
                return Err("error: no .sql or .json inputs found.".into());
            }
            let root = common_root(&files);
            files
                .iter()
                .map(|f| Item { path: f.clone(), name: display_name(f, &root), row: None })
                .filter(|i| only.as_ref().is_none_or(|o| o.contains(&i.name)))
                .collect()
        }
    };
    if pinned {
        let plans: Vec<String> = items
            .iter()
            .filter(|i| suffix(&i.path) == ".json")
            .take(3)
            .map(|i| i.path.to_string_lossy().into_owned())
            .collect();
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
    Ok(Env { axes, frontend, prover, ss, fuzz, lean, items })
}

/// GiB as bytes, for an address-space cap.
fn gib(g: Option<f64>) -> Option<u64> {
    g.filter(|g| *g > 0.0).map(|g| (g * 1024.0 * 1024.0 * 1024.0) as u64)
}

/// The names already in a `--jsonl` file, for `--resume`. A torn last line -- the run that wrote it
/// was killed mid-write -- is not a case, and is run again.
fn names_in_jsonl(path: &Path) -> HashSet<String> {
    let Ok(text) = std::fs::read_to_string(path) else { return HashSet::new() };
    text.lines()
        .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
        .filter_map(|v| v.get("name").and_then(|n| n.as_str()).map(str::to_string))
        .collect()
}

/// `--jsonl`: each case as one line, written when its last asked axis has answered.
struct Stream(Option<Mutex<std::fs::File>>);

impl Stream {
    fn emit(&self, case: &Case) {
        let Some(f) = &self.0 else { return };
        if let Ok(line) = serde_json::to_string(case) {
            let mut f = f.lock().unwrap_or_else(|e| e.into_inner());
            let _ = writeln!(f, "{line}");
            let _ = f.flush();
        }
    }
}

/// A case back as the item it was run from, for the retry pass.
fn item_of(case: &Case) -> Item {
    Item { path: PathBuf::from(&case.path), name: case.name.clone(), row: case.row.clone() }
}

/// How the retry pass runs, for its progress line.
fn retry_how(args: &Args) -> String {
    let budget = args.retry_timeout.map(|t| format!(", {}s each", report::fmt_secs(t))).unwrap_or_default();
    if args.retry_jobs > 1 {
        format!("{} at a time{budget}", args.retry_jobs)
    } else {
        format!("serially{budget}")
    }
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
fn run_all(items: &[Item], run: &(dyn Fn(&Item) -> Case + Sync), jobs: usize, mut done: impl FnMut(Case)) {
    let next = AtomicUsize::new(0);
    let (tx, rx) = mpsc::channel();
    std::thread::scope(|s| {
        for _ in 0..jobs.max(1).min(items.len().max(1)) {
            let tx = tx.clone();
            let next = &next;
            s.spawn(move || loop {
                let k = next.fetch_add(1, Ordering::Relaxed);
                let Some(item) = items.get(k) else { break };
                if tx.send(run(item)).is_err() {
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
    let mut files: Vec<Item> = env.items;

    // `--jsonl`, and with `--resume` only what it does not hold yet.
    let stream = match &args.jsonl {
        None => Stream(None),
        Some(p) => {
            if args.resume {
                let done = names_in_jsonl(Path::new(p));
                let before = files.len();
                files.retain(|i| !done.contains(&i.name));
                if !args.quiet {
                    println!("{}", c.dim(&format!("resuming {p}: {} of {before} case(s) left", files.len())));
                }
            }
            let f = std::fs::OpenOptions::new().create(true).append(args.resume).write(true).truncate(!args.resume).open(p);
            // A killed run can leave a torn last line with no newline; the first case appended must
            // not be glued onto it, or it is lost along with the torn one.
            let torn = args.resume && std::fs::read(p).is_ok_and(|b| b.last().is_some_and(|c| *c != b'\n'));
            match f {
                Ok(mut f) => {
                    if torn {
                        let _ = writeln!(f);
                    }
                    Stream(Some(Mutex::new(f)))
                }
                Err(e) => {
                    eprintln!("error: {p}: {e}");
                    return 2;
                }
            }
        }
    };
    let ss_timeout_ms = args.sqleq_solver_timeout.filter(|t| *t > 0).unwrap_or((args.timeout * 1000.0) as u64);

    // Absolute: a case's directory under it is also the working directory its backends run in, so
    // a path relative to ours would name somewhere else to them -- the sqleq-solver job a portfolio
    // packages there among them.
    let keep_dir = match args.keep.as_ref().map(PathBuf::from) {
        None => None,
        Some(k) => {
            if let Err(e) = std::fs::create_dir_all(&k) {
                eprintln!("error: {}: {e}", k.display());
                return 2;
            }
            Some(k.canonicalize().unwrap_or_else(|_| crate::util::abspath(&k)))
        }
    };
    // Each case's directory is emptied when the case starts, so two cases must not share one -- the
    // second would empty the first's while it runs -- and none may hold an input.
    if let Some(k) = &keep_dir {
        let mut seen: std::collections::HashMap<String, &str> = std::collections::HashMap::new();
        for i in &files {
            if let Some(other) = seen.insert(crate::case::keep_name(&i.name), &i.name) {
                eprintln!("error: --keep would put {other} and {} in one directory; rename one of them", i.name);
                return 2;
            }
        }
        let dirs: HashSet<PathBuf> = seen.keys().map(|n| k.join(n)).collect();
        for i in &files {
            let p = i.path.canonicalize().unwrap_or_else(|_| crate::util::abspath(&i.path));
            if let Some(d) = p.ancestors().skip(1).find(|a| dirs.contains(*a)) {
                eprintln!("error: --keep would empty {}, which holds the input {}", d.display(), i.path.display());
                return 2;
            }
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
                    let d = k.join("sqlsolver");
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

    let name_w = files.iter().map(|i| i.name.chars().count()).max().unwrap_or(10).min(60);
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
        catalog: args.catalog.map(|c| c.name().to_string()),
        qed_mem: gib(args.qed_mem_gib),
    };
    // The retry pass's own tier: a longer budget, when one was given, for what the first ran out of.
    let retry_stage = Stage {
        timeout: args.retry_timeout.unwrap_or(args.timeout),
        smt_timeout_ms: args.retry_smt_timeout.or(args.smt_timeout),
        ..stage.clone()
    };
    // The pass after which a case says nothing more, and is written to `--jsonl`.
    let last_pass = if args.portfolio {
        "main"
    } else if env.lean.is_some() {
        "lean"
    } else if env.fuzz.is_some() {
        "fuzz"
    } else if env.ss.is_some() {
        "solver"
    } else {
        "main"
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
        catalog: args.catalog.map(|c| c.name()),
        qed_mem: gib(args.qed_mem_gib),
        ss_mem: gib(args.sqleq_solver_mem_gib),
    };
    let retry_pctx = portfolio::Ctx {
        timeout: args.retry_timeout.unwrap_or(args.timeout),
        smt_timeout_ms: args.retry_smt_timeout.or(args.smt_timeout),
        ..pctx
    };
    let run_one = |item: &Item| {
        if args.portfolio {
            portfolio::run_case(item, &pctx)
        } else {
            run_case(item, &stage)
        }
    };
    // Whether the retry pass will take a case: until it has, the case is not final.
    let will_retry = |case: &Case| {
        !args.no_retry
            && if args.portfolio {
                portfolio::worth_retrying(case)
            } else {
                [PANIC, TIMEOUT, ERROR].contains(&case.status.as_str()) && env.frontend.is_some()
            }
    };
    if env.frontend.is_none() && !args.portfolio {
        // Only axes that read the pair themselves: nothing to lower.
        for item in &files {
            let mut x = Case::of(item);
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
            if last_pass == "main" && !will_retry(&case) {
                stream.emit(&case);
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
                println!("{}", c.dim(&format!("  re-running {} undecided case(s), {}…", retry.len(), retry_how(&args))));
            }
            retried = retry.len();
            let items: Vec<Item> = retry.iter().map(|&i| item_of(&cases[i])).collect();
            let at: std::collections::HashMap<String, usize> = retry.iter().map(|&i| (cases[i].name.clone(), i)).collect();
            let mut fresh = Vec::new();
            run_all(&items, &|item: &Item| portfolio::run_case(item, &retry_pctx), args.retry_jobs, |new| fresh.push(new));
            for mut new in fresh {
                let i = at[&new.name];
                if let Some(o) = new.portfolio.as_mut() {
                    o.retried = true;
                }
                if new.portfolio.as_ref().is_some_and(|o| portfolio::decisive(&o.verdict)) {
                    if !args.quiet {
                        report::print_line(c, &new, name_w);
                    }
                    cases[i] = new;
                }
                stream.emit(&cases[i]);
            }
        }
    } else if !args.no_retry && env.frontend.is_some() {
        let retry: Vec<usize> = (0..cases.len()).filter(|&i| transient(&cases[i].status)).collect();
        if !retry.is_empty() {
            if !args.quiet && live {
                progress(&" ".repeat(30));
            }
            if !args.quiet {
                println!("{}", c.dim(&format!("  re-running {} transient failure(s) {}…", retry.len(), retry_how(&args))));
            }
            let items: Vec<Item> = retry.iter().map(|&i| item_of(&cases[i])).collect();
            let at: std::collections::HashMap<String, usize> = retry.iter().map(|&i| (cases[i].name.clone(), i)).collect();
            let mut fresh = Vec::new();
            run_all(&items, &|item: &Item| run_case(item, &retry_stage), args.retry_jobs, |new| fresh.push(new));
            for new in fresh {
                let i = at[&new.name];
                let old = &cases[i];
                let better = !transient(&new.status) || status_rank(&new.status) < status_rank(&old.status);
                if better {
                    if !args.quiet && !pinned_mode {
                        report::print_case_line(c, &new, name_w);
                    }
                    cases[i] = new;
                }
                if last_pass == "main" {
                    stream.emit(&cases[i]);
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
            let how = if args.sqleq_solver_jobs > 1 {
                format!("{} drivers side by side", args.sqleq_solver_jobs)
            } else {
                "sequentially".to_string()
            };
            println!(
                "{}",
                c.dim(&format!("  asking {} about {n} case(s), {how}, {ss_timeout_ms}ms/row…", solver::name(&driver.imp)))
            );
        }
        let t1 = Instant::now();
        let mem = gib(args.sqleq_solver_mem_gib);
        ss_stats = solver::run_second_opinion(&mut cases, dir, driver, ss_timeout_ms, args.sqleq_solver_jobs, mem);
        ss_stats.wall_s = Some(round(t1.elapsed().as_secs_f64(), 3));
        if last_pass == "solver" {
            cases.iter().for_each(|x| stream.emit(x));
        }
    }

    // The fuzz axis reads the pair files itself, so it is independent of the passes above --
    // though not of the pair, which is the point.
    let mut fuzz_stats = None;
    let sql_cases = cases.iter().filter(|x| x.has_pair()).count();
    if args.portfolio {
        fuzz_stats = env.fuzz.as_ref().map(|_| fuzz::Stats { rows: sql_cases, wall_s: None });
    } else if let Some(fz) = &env.fuzz {
        if !args.quiet {
            if live {
                progress(&" ".repeat(30));
            }
            println!("{}", c.dim(&format!("  asking sqleq-fuzz about {sql_cases} case(s)…")));
        }
        let done = |x: &Case| {
            if last_pass == "fuzz" {
                stream.emit(x);
            }
        };
        fuzz_stats = Some(fuzz::run_fuzz(&mut cases, fz, args.jobs, args.timeout, &done));
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
            println!("{}", c.dim(&format!("  asking sqleq-lean about {sql_cases} case(s)…")));
        }
        let plan = args.lean_replay_plan.as_deref().map(Path::new);
        lean_stats = Some(lean::run_lean(&mut cases, lb, args.jobs, args.timeout, keep_dir.as_deref(), plan));
        cases.iter().for_each(|x| stream.emit(x));
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
    // A proof and a counterexample on one pair: one of the backends is wrong. A portfolio's verdict
    // says so; a run without one compares the very same answers here, so that no mode passes over
    // what --portfolio fails on.
    let mut alarms: Vec<report::Alarm> = cases
        .iter()
        .enumerate()
        .filter_map(|(i, x)| {
            let by = match &x.portfolio {
                Some(o) => (o.verdict == portfolio::ALARM).then(|| o.by.clone()),
                None => portfolio::alarm(x, &axes),
            };
            by.map(|by| report::Alarm { case: i, by, known: false })
        })
        .collect();
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
        // A pair whose wrong answer is pinned `!known-unsound` is a known bug reproducing, and its
        // alarm passes as that pin does. Any other alarm fails here even where every pin holds: a
        // pair stated under a gather binding has no index-binding truth for the two to contradict.
        for a in &mut alarms {
            a.known = pinned.iter().filter(|p| p.case == a.case).any(|p| {
                p.judgements.iter().any(|j| j.state == crate::suite::KNOWN && a.by.contains(&j.axis))
            });
        }
        report::print_pinned(c, &pinned, &cases, &axes);
        if args.bless {
            println!("  {} {} file(s)", c.bold("blessed"), blessed.len());
            for name in &blessed {
                println!("{}", c.dim(&format!("    {name}")));
            }
        }
        report::print_alarms(c, &cases, &alarms);
    } else {
        if env.frontend.is_some() {
            report::print_summary(c, &cases, wall, axes.contains(&"qed"));
        }
        let imp = env.ss.as_ref().map_or("sqleq-solver", |d| d.imp.as_str());
        report::print_second_opinion(c, &cases, &ss_stats, imp, axes.contains(&"qed"));
        if let Some(st) = &fuzz_stats {
            report::print_fuzz(c, &cases, st);
        }
        if let Some(st) = &lean_stats {
            report::print_lean(c, &cases, st);
        }
        if args.portfolio {
            report::print_portfolio(c, &cases, &backends(&axes), args.timeout, retried);
        } else {
            report::print_alarms(c, &cases, &alarms);
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
        alarms: alarms.iter().filter(|a| !a.known).map(|a| cases[a.case].name.clone()).collect(),
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

    // A soundness alarm fails every policy, with or without --portfolio: it says one of the
    // backends is wrong, which no report should pass over in silence.
    if alarms.iter().any(|a| !a.known) {
        return 1;
    }
    let verdict_is = |x: &Case, v: &str| x.portfolio.as_ref().is_some_and(|o| o.verdict == v);
    if args.portfolio {
        // The combined verdict decides.
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
        // `no-proof` is not a failure to begin with. (Its proof can still be half of an alarm,
        // above, which turns a green run red.)
        Expect::Equivalent => i32::from(!cases.iter().all(|x| x.status == PROVABLE)),
    }
}
