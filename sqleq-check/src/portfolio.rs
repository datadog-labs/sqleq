// Unless explicitly stated otherwise all files in this repository are licensed under the
// Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/).
// Copyright 2026-Present Datadog, Inc.

//! Portfolio solving: every backend at once on each case, under one deadline, and one verdict.
//!
//! `--portfolio` starts sqleq-fuzz and Lean on a case the moment it is picked up, beside the
//! frontend, and the QED prover and sqleq-solver as soon as the frontend has lowered and packaged
//! the pair. Every one of them gets whatever is left of the case's `--timeout`, and what is still
//! running when it passes is killed with its process group. No backend is stopped because another
//! answered: a proof and a counterexample on the same pair is a soundness alarm in one of them, and
//! a run that stopped at the first answer could not see it.
//!
//! The verdict is read off the same per-axis words `--expect pinned` judges ([`crate::pinned`]),
//! through the suite's own tables of which word claims, evidences or refutes equivalence under
//! which parameter binding, so a new axis word needs no change here.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::time::Instant;

use serde::Serialize;
use serde_json::Value;

use crate::axes::{fuzz, lean, solver};
use crate::case::{self, Case, Workdir, ERROR, LOWERED, PANIC, TIMEOUT as CASE_TIMEOUT};
use crate::discover::SsDriver;
use crate::inputs::{first_row_name, CorpusRow, Item};
use crate::suite::{self, AXES, BINDINGS, GATHER, GATHER_GENERATED, INDEX};
use crate::util::tail;

/// A proof and a counterexample under the same binding: one of the two backends is wrong.
pub const ALARM: &str = "alarm";
/// sqleq-fuzz found an instance on which the two sides differ.
pub const NOT_EQUIVALENT: &str = "not-equivalent";
/// A prover proved it under index binding -- `$N` on one side is `$N` on the other -- or the
/// frontend found the two sides one query (`emit-reflexive`, or refused but `reflexive`).
pub const EQUIVALENT: &str = "equivalent";
/// Only Lean proved it, under the gather rule: a different claim (docs/LEAN.md).
pub const EQUIVALENT_GATHER: &str = "equivalent-gather";
/// Only Lean proved it, under the gather rule's weaker generated form.
pub const EQUIVALENT_GATHER_GENERATED: &str = "equivalent-gather-generated";
/// Nothing decisive, and a backend was still running at the deadline: more time might decide it.
pub const TIMEOUT: &str = "timeout";
/// Nothing decisive, and every backend finished.
pub const UNDECIDED: &str = "undecided";
pub const ORDER: [&str; 7] =
    [ALARM, NOT_EQUIVALENT, EQUIVALENT, EQUIVALENT_GATHER, EQUIVALENT_GATHER_GENERATED, TIMEOUT, UNDECIDED];

/// Every verdict but these two says something about the pair.
pub fn decisive(v: &str) -> bool {
    v != TIMEOUT && v != UNDECIDED
}

/// What the portfolio made of one case.
#[derive(Clone, Debug, Default, Serialize)]
pub struct Outcome {
    pub verdict: String,
    /// The axes the verdict rests on, in the order they answered.
    pub by: Vec<String>,
    /// The axes still running when the deadline passed.
    pub pending: Vec<String>,
    /// Axis -> seconds from the case's start to that backend's answer.
    pub done: BTreeMap<String, f64>,
    /// Seconds to the first answer the verdict rests on.
    pub first_s: Option<f64>,
    /// Whether this is the serial re-run of a case the parallel pass left undecided.
    pub retried: bool,
}

/// The verdict on one case from what each axis said (`observed`: axis -> (word, note), as
/// [`crate::pinned::observe`] reports it): the verdict, the axes it rests on (in canonical order),
/// and the axes cut off at the deadline.
pub fn verdict(observed: &HashMap<String, (String, String)>) -> (&'static str, Vec<String>, Vec<String>) {
    let word = |a: &str| observed.get(a).map(|(w, _)| w.as_str());
    let saying = |binding: &str, words: &dyn Fn(&str) -> &'static [&'static str]| -> Vec<String> {
        AXES.iter()
            .filter(|a| suite::axis_bindings(a).contains(&binding))
            .filter(|a| word(a).is_some_and(|w| words(a).contains(&w)))
            .map(|a| a.to_string())
            .collect()
    };
    let pending: Vec<String> =
        AXES.iter().filter(|a| word(a) == Some("timeout")).map(|a| a.to_string()).collect();
    // An answer can contradict another only under the binding both answer under; a Lean proof and
    // a fuzz counterexample are about different relations between the two sides' parameters.
    for b in BINDINGS {
        let claims = saying(b, &|a| suite::claims_equivalent(a, b));
        let refuted = saying(b, &suite::refutes);
        if !claims.is_empty() && !refuted.is_empty() {
            let by = AXES.iter().map(|a| a.to_string()).filter(|a| claims.contains(a) || refuted.contains(a)).collect();
            return (ALARM, by, pending);
        }
    }
    let refuted = saying(INDEX, &suite::refutes);
    if !refuted.is_empty() {
        return (NOT_EQUIVALENT, refuted, pending);
    }
    for (b, v) in [(INDEX, EQUIVALENT), (GATHER, EQUIVALENT_GATHER), (GATHER_GENERATED, EQUIVALENT_GATHER_GENERATED)] {
        let evidence = saying(b, &|a| suite::evidence_equivalent(a, b));
        if !evidence.is_empty() {
            return (v, evidence, pending);
        }
    }
    (if pending.is_empty() { UNDECIDED } else { TIMEOUT }, Vec::new(), pending)
}

/// The axes of an alarm on `case`, from what each axis in `axes` said of it: under one binding, one
/// claims equivalence and another refutes it. Read off the same answers as [`verdict`], so a run
/// without `--portfolio` sees exactly the alarms a portfolio would.
pub fn alarm(case: &Case, axes: &[&str]) -> Option<Vec<String>> {
    let (v, by, _) = verdict(&crate::pinned::observe(case, axes));
    (v == ALARM).then_some(by)
}

/// Everything a portfolio case needs besides the case itself.
pub struct Ctx<'a> {
    pub axes: &'a [&'static str],
    pub frontend: Option<&'a str>,
    pub prover: Option<&'a str>,
    pub ss: Option<&'a SsDriver>,
    /// `--sqleq-solver-timeout`: a cap within the deadline, when one was given.
    pub ss_cap_ms: Option<u64>,
    pub fuzz: Option<&'a str>,
    pub lean: Option<&'a str>,
    pub timeout: f64,
    pub smt_timeout_ms: Option<u64>,
    pub keep_dir: Option<&'a Path>,
    /// `--catalog`.
    pub catalog: Option<&'a str>,
    /// `--qed-mem-gib` and `--sqleq-solver-mem-gib`, in bytes.
    pub qed_mem: Option<u64>,
    pub ss_mem: Option<u64>,
}

/// Seconds left before `deadline`; all the time in the world when there is none (`-t inf`).
fn left(deadline: Option<Instant>) -> f64 {
    deadline.map_or(f64::INFINITY, |d| d.saturating_duration_since(Instant::now()).as_secs_f64())
}

enum SsAnswer {
    Row(Value),
    CutOff,
    Failed(String),
}

/// sqleq-solver on one packaged job, within what is left of the deadline. Its own cap is set a
/// margin short of the deadline, so it can write a `killed` row itself; the process-group kill at
/// the deadline is the backstop for a row that will not stop.
fn ss_one(
    driver: &SsDriver,
    name: &str,
    job: &Path,
    out: &Path,
    cap_ms: Option<u64>,
    mem: Option<u64>,
    deadline: Option<Instant>,
) -> SsAnswer {
    let remaining = left(deadline);
    let remaining_ms = (remaining * 1000.0) as u64;
    if remaining_ms == 0 {
        return SsAnswer::CutOff;
    }
    let margin = (remaining_ms / 10).clamp(50, 2000);
    let cap = remaining_ms.saturating_sub(2 * margin).max(1).min(cap_ms.unwrap_or(u64::MAX));
    let mut argv = driver.cmd.clone();
    argv.extend([
        job.to_string_lossy().into_owned(),
        out.to_string_lossy().into_owned(),
        format!("--timeout-ms={cap}"),
        format!("--grace-ms={margin}"),
    ]);
    let r = crate::proc::run_limited(&argv, Some(&driver.cwd), &driver.env, Some(remaining), mem);
    if let Some(row) = solver::answered(out).remove(name) {
        return SsAnswer::Row(row);
    }
    if r.timed_out {
        return SsAnswer::CutOff;
    }
    let why = tail(&r.err);
    SsAnswer::Failed(if why.is_empty() { format!("sqleq-solver exit {}", r.rc) } else { why })
}

enum LeanAnswer {
    Record(Value),
    CutOff,
    Missing(String),
}

/// sqleq-lean on one pair, within what is left of the deadline: a pair file by its path, a corpus
/// row as a one-row CSV, whose record its corpus mode keys by the name of row 0. Its temporary
/// directory is put in the case's own, so a killed run leaves nothing behind.
fn lean_one(bin: &str, path: &str, row: Option<&CorpusRow>, workdir: &Path, deadline: Option<Instant>) -> LeanAnswer {
    let remaining = left(deadline);
    if remaining <= 0.0 {
        return LeanAnswer::CutOff;
    }
    let out = workdir.join("lean.json");
    let mut argv = vec![
        bin.to_string(),
        "--full-names".into(),
        "--json".into(),
        out.to_string_lossy().into_owned(),
        "--jobs".into(),
        "1".into(),
        "--timeout".into(),
        (remaining.ceil() as u64).max(1).to_string(),
    ];
    let key = match row {
        None => {
            argv.push(path.to_string());
            path.to_string()
        }
        Some(r) => match r.write_csv(workdir) {
            Ok(csv) => {
                argv.extend(["--csv".into(), csv.to_string_lossy().into_owned()]);
                first_row_name()
            }
            Err(e) => return LeanAnswer::Missing(format!("cannot write the row: {e}")),
        },
    };
    let env = [("TMPDIR".to_string(), workdir.to_string_lossy().into_owned())];
    let r = crate::proc::run(&argv, None, &env, Some(remaining));
    if r.timed_out {
        return LeanAnswer::CutOff;
    }
    let got = std::fs::read_to_string(&out).ok().and_then(|t| serde_json::from_str::<Value>(&t).ok());
    match got.as_ref().and_then(|g| g.get(&key)) {
        Some(rec) => LeanAnswer::Record(rec.clone()),
        None if got.is_some() => LeanAnswer::Missing("no record".into()),
        None => {
            let why = tail(&r.err);
            LeanAnswer::Missing(if why.is_empty() { "no output".into() } else { why })
        }
    }
}

/// One case through every backend in `ctx` at once.
pub fn run_case(item: &Item, ctx: &Ctx) -> Case {
    let t0 = Instant::now();
    let deadline = crate::proc::deadline_after(t0, ctx.timeout);
    let mut case = Case::of(item);
    let (src, name) = (item.path.as_path(), item.name.as_str());
    let wd = match Workdir::new(ctx.keep_dir, name) {
        Ok(w) => w,
        Err(e) => {
            case.message = format!("cannot create a work directory: {e}");
            case.portfolio = Some(Outcome { verdict: UNDECIDED.into(), ..Outcome::default() });
            return case;
        }
    };
    let workdir: PathBuf = wd.path.clone();
    let path = case.path.clone();
    let row = case.row.clone();
    let is_sql = case.has_pair();
    let mut done: BTreeMap<String, f64> = BTreeMap::new();
    let since = |t0: Instant| t0.elapsed().as_secs_f64();

    let (fz, ln) = std::thread::scope(|s| {
        // The two axes that read the pair file themselves start at once, beside the frontend.
        let fz = ctx.fuzz.filter(|_| is_sql).map(|bin| {
            let (path, row) = (&path, &row);
            s.spawn(move || {
                let remaining = left(deadline);
                let pair = match row {
                    Some(r) => fuzz::Pair::Row(r),
                    None => fuzz::Pair::File(path),
                };
                let r = if remaining > 0.0 {
                    fuzz::fuzz_one(bin, pair, remaining)
                } else {
                    fuzz::Answer { word: "timeout".into(), note: String::new(), ms: 0, raw: None }
                };
                (r, since(t0))
            })
        });
        let ln = ctx.lean.filter(|_| is_sql).map(|bin| {
            let (path, row, workdir) = (&path, &row, &workdir);
            s.spawn(move || (lean_one(bin, path, row.as_ref(), workdir, deadline), since(t0)))
        });

        match ctx.frontend {
            None => case.status = LOWERED.into(),
            Some(fe) => {
                let json = if left(deadline) > 0.0 {
                    case::lower(&mut case, src, &workdir, fe, ctx.catalog, left(deadline))
                } else {
                    case.status = CASE_TIMEOUT.into();
                    case.message = "frontend timed out".into();
                    None
                };
                done.insert("frontend".into(), since(t0));
                if let Some(json) = json {
                    // The second opinion's job, packaged from the very plan the prover reads.
                    let ss = ctx.ss.and_then(|d| {
                        let job = workdir.join("ss.job.jsonl");
                        let out = workdir.join("ss.results.jsonl");
                        if left(deadline) > 0.0 {
                            case::package(&mut case, fe, &workdir, &json, &job, left(deadline));
                        } else {
                            case.s_bucket = Some(solver::TIMEOUT.into());
                            case.s_note = "still running at the deadline".into();
                        }
                        case.s_bucket.is_none().then(|| {
                            let name = name.to_string();
                            s.spawn(move || (ss_one(d, &name, &job, &out, ctx.ss_cap_ms, ctx.ss_mem, deadline), since(t0)))
                        })
                    });
                    match ctx.prover {
                        Some(p) => {
                            if left(deadline) > 0.0 {
                                case::prove(&mut case, &workdir, &json, p, left(deadline), ctx.smt_timeout_ms, ctx.qed_mem);
                            } else {
                                case.status = CASE_TIMEOUT.into();
                                case.message = "prover timed out".into();
                            }
                            done.insert("qed".into(), since(t0));
                        }
                        None => case.status = LOWERED.into(),
                    }
                    if let Some(h) = ss {
                        if let Ok((answer, t)) = h.join() {
                            match answer {
                                SsAnswer::Row(row) => solver::attach(&mut case, &row),
                                SsAnswer::CutOff => {
                                    case.s_bucket = Some(solver::TIMEOUT.into());
                                    case.s_note = "still running at the deadline".into();
                                }
                                SsAnswer::Failed(why) => {
                                    case.s_bucket = Some(solver::ERROR.into());
                                    case.s_note = why;
                                }
                            }
                            done.insert("sqleq-solver".into(), t);
                        }
                    }
                }
            }
        }
        (fz.and_then(|h| h.join().ok()), ln.and_then(|h| h.join().ok()))
    });

    if let Some((answer, t)) = fz {
        answer.attach(&mut case);
        done.insert("fuzz".into(), t);
    }
    if let Some((answer, t)) = ln {
        match answer {
            LeanAnswer::Record(rec) => lean::attach(&mut case, &rec),
            LeanAnswer::CutOff => case.l_verdict = Some("timeout".into()),
            LeanAnswer::Missing(why) => (case.l_verdict, case.l_reason) = (Some("missing".into()), why),
        }
        done.insert("lean".into(), t);
    }
    case.wall = since(t0);
    case.portfolio = Some(outcome(&case, ctx.axes, done));
    case
}

fn outcome(case: &Case, axes: &[&str], done: BTreeMap<String, f64>) -> Outcome {
    let (v, mut by, pending) = verdict(&crate::pinned::observe(case, axes));
    let at = |a: &String| done.get(a).copied().unwrap_or(f64::INFINITY);
    by.sort_by(|a, b| at(a).total_cmp(&at(b)));
    let first_s = by.first().and_then(|a| done.get(a).copied());
    Outcome { verdict: v.into(), by, pending, done, first_s, retried: false }
}

/// Whether a case the parallel pass left undecided is worth one serial re-run: it ran out of time,
/// or a backend failed in a way that load can cause. A refusal is deterministic and never is.
pub fn worth_retrying(case: &Case) -> bool {
    let Some(o) = &case.portfolio else { return false };
    match o.verdict.as_str() {
        TIMEOUT => true,
        UNDECIDED => {
            case.status == PANIC
                || case.status == ERROR
                || matches!(case.s_bucket.as_deref(), Some(solver::ERROR) | Some(solver::MISSING))
        }
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(pairs: &[(&str, &str)]) -> (&'static str, Vec<String>, Vec<String>) {
        let observed = pairs.iter().map(|(a, w)| (a.to_string(), (w.to_string(), String::new()))).collect();
        verdict(&observed)
    }

    /// (label, axis -> word, expected verdict)
    type Row<'a> = (&'a str, Vec<(&'a str, &'a str)>, &'a str);

    #[test]
    fn verdict_rows() {
        let rows: Vec<Row> = vec![
            ("a proof", vec![("frontend", "emit"), ("qed", "proved"), ("fuzz", "no-counterexample")], EQUIVALENT),
            ("the second prover's proof", vec![("qed", "no-proof"), ("sqleq-solver", "proved")], EQUIVALENT),
            ("reflexive is still a proof", vec![("frontend", "emit-reflexive"), ("qed", "proved-literal")], EQUIVALENT),
            // Settled by reflexivity although the frontend refused it: no plan, and no prover needed.
            ("refused, but reflexive", vec![("frontend", "reflexive"), ("qed", "no-plan"), ("fuzz", "no-counterexample")], EQUIVALENT),
            ("reflexive and a counterexample", vec![("frontend", "reflexive"), ("fuzz", "counterexample")], ALARM),
            // Identical IR settles the pair even when the prover fails on it.
            ("lowered alike, prover timed out", vec![("frontend", "emit-reflexive"), ("qed", "timeout")], EQUIVALENT),
            ("a plain refusal decides nothing", vec![("frontend", "refuse:unsupported"), ("qed", "no-plan")], UNDECIDED),
            ("a counterexample", vec![("qed", "no-proof"), ("fuzz", "counterexample")], NOT_EQUIVALENT),
            ("a proof and a counterexample", vec![("qed", "proved"), ("fuzz", "counterexample")], ALARM),
            ("a literal proof and a counterexample", vec![("sqleq-solver", "proved-literal"), ("fuzz", "counterexample")], ALARM),
            // Different bindings: the Lean proof relates the parameters by the gather rule.
            ("lean and fuzz", vec![("lean", "proved-gather"), ("fuzz", "counterexample")], NOT_EQUIVALENT),
            ("lean alone", vec![("frontend", "refuse:parameter-misaligned"), ("lean", "proved-gather")], EQUIVALENT_GATHER),
            ("lean generated", vec![("lean", "proved-gather-generated")], EQUIVALENT_GATHER_GENERATED),
            // Possibly vacuous: a claim, not evidence.
            ("lean no-witness", vec![("lean", "no-witness"), ("fuzz", "no-counterexample")], UNDECIDED),
            ("nothing decisive, all done", vec![("qed", "no-proof"), ("fuzz", "no-counterexample")], UNDECIDED),
            ("nothing decisive, one cut off", vec![("qed", "timeout"), ("fuzz", "no-counterexample")], TIMEOUT),
            ("the frontend cut off", vec![("frontend", "timeout"), ("qed", "no-plan"), ("fuzz", "no-counterexample")], TIMEOUT),
            ("a refusal", vec![("frontend", "refuse:unsupported"), ("qed", "no-plan"), ("fuzz", "error")], UNDECIDED),
            ("a proof beats a cut-off", vec![("qed", "proved"), ("sqleq-solver", "timeout")], EQUIVALENT),
        ];
        for (label, observed, want) in rows {
            assert_eq!(v(&observed).0, want, "{label}");
        }
    }

    #[test]
    fn verdicts_say_what_they_rest_on_and_what_was_cut_off() {
        assert_eq!(
            v(&[("qed", "proved"), ("sqleq-solver", "proved"), ("fuzz", "timeout")]),
            (EQUIVALENT, vec!["qed".to_string(), "sqleq-solver".to_string()], vec!["fuzz".to_string()])
        );
        assert_eq!(
            v(&[("qed", "proved"), ("sqleq-solver", "no-proof"), ("fuzz", "counterexample")]).1,
            ["fuzz", "qed"],
            "an alarm names both sides"
        );
    }

    #[test]
    fn every_verdict_is_ordered_and_decisive_ones_are_marked() {
        assert!(ORDER.iter().all(|o| decisive(o) == !matches!(*o, TIMEOUT | UNDECIDED)));
    }
}
