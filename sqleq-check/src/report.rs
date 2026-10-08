// Unless explicitly stated otherwise all files in this repository are licensed under the
// Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/).
// Copyright 2026-Present Datadog, Inc.

//! Everything the harness prints or writes.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use serde::Serialize;

use crate::axes::{fuzz, lean, solver};
use crate::portfolio;
use crate::case::{Case, LOWERED, PANIC, PROVABLE, REFUSED, STATUS_ORDER, TIMEOUT, UNPROVABLE};
use crate::pinned::Pinned;
use crate::suite::{self, Judgement};

#[derive(Clone, Copy)]
pub struct Color {
    pub on: bool,
}

impl Color {
    fn w(&self, code: &str, s: &str) -> String {
        if self.on {
            format!("\x1b[{code}m{s}\x1b[0m")
        } else {
            s.to_string()
        }
    }
    pub fn green(&self, s: &str) -> String {
        self.w("32", s)
    }
    pub fn red(&self, s: &str) -> String {
        self.w("31", s)
    }
    pub fn yellow(&self, s: &str) -> String {
        self.w("33", s)
    }
    pub fn dim(&self, s: &str) -> String {
        self.w("2", s)
    }
    pub fn bold(&self, s: &str) -> String {
        self.w("1", s)
    }
}

fn rule(c: Color) -> String {
    c.dim(&format!("  {}", "─".repeat(40)))
}

pub fn fmt_status(c: Color, status: &str) -> String {
    let glyph = match status {
        PROVABLE => "✓",
        UNPROVABLE => "✗",
        TIMEOUT => "⏱",
        REFUSED => "⚠",
        PANIC => "💥",
        LOWERED => "·",
        _ => "?",
    };
    let s = format!("{glyph} {status}");
    match status {
        PROVABLE => c.green(&s),
        UNPROVABLE | TIMEOUT | REFUSED => c.yellow(&s),
        LOWERED => c.dim(&s),
        _ => c.red(&s),
    }
}

pub fn print_case_line(c: Color, case: &Case, name_w: usize) {
    let mut extra = Vec::new();
    if case.complete_fragment {
        extra.push("complete-frag".to_string());
    }
    if case.smt_timed_out {
        extra.push(c.yellow("smt-timeout"));
    }
    if case.nontrivial_perms {
        extra.push("perm".to_string());
    }
    let tag = if extra.is_empty() { String::new() } else { format!("  {}", c.dim(&extra.join(" "))) };
    // Show the last non-empty line -- for a refusal that is the frontend's own one-line reason,
    // which is the useful part.
    let last = suite::splitlines(&case.message).into_iter().map(str::trim).rfind(|l| !l.is_empty());
    let msg = last.map(|l| format!("  {}", c.dim(&format!("— {l}")))).unwrap_or_default();
    println!(
        "  {:<22} {:<name_w$}  {}{tag}{msg}",
        fmt_status(c, &case.status),
        case.name,
        c.dim(&format!("{:6.2}s", case.wall))
    );
}

/// A case's line as it finishes: its portfolio verdict when it has one, else its status.
pub fn print_line(c: Color, case: &Case, name_w: usize) {
    match &case.portfolio {
        Some(o) => print_portfolio_line(c, case, o, name_w),
        None => print_case_line(c, case, name_w),
    }
}

pub fn fmt_verdict(c: Color, v: &str) -> String {
    let s = format!(
        "{} {v}",
        match v {
            portfolio::ALARM => "‼",
            portfolio::NOT_EQUIVALENT => "✗",
            portfolio::TIMEOUT => "⏱",
            portfolio::UNDECIDED => "·",
            _ => "✓",
        }
    );
    match v {
        portfolio::ALARM => c.red(&c.bold(&s)),
        portfolio::NOT_EQUIVALENT | portfolio::TIMEOUT => c.yellow(&s),
        portfolio::UNDECIDED => c.dim(&s),
        _ => c.green(&s),
    }
}

/// The verdict, which backends it rests on and when each answered, and which were cut off.
fn print_portfolio_line(c: Color, case: &Case, o: &portfolio::Outcome, name_w: usize) {
    let mut tail = String::new();
    if !o.by.is_empty() {
        let at: Vec<String> =
            o.by.iter().map(|a| format!("{a} {:.2}s", o.done.get(a).copied().unwrap_or(0.0))).collect();
        tail += &format!("  {}", c.dim(&format!("by {}", at.join(", "))));
    }
    if !o.pending.is_empty() {
        tail += &format!("  {}", c.yellow(&format!("⏱ {}", o.pending.join(", "))));
    }
    if o.verdict == portfolio::ALARM {
        tail += &format!("  {}", c.red(&format!("— {}", alarm_words(case, &o.by))));
    } else if o.verdict == portfolio::NOT_EQUIVALENT && !case.f_note.is_empty() {
        tail += &format!("  {}", c.dim(&format!("— {}", case.f_note)));
    } else if !portfolio::decisive(&o.verdict) {
        let last = suite::splitlines(&case.message).into_iter().map(str::trim).rfind(|l| !l.is_empty());
        if let Some(l) = last {
            tail += &format!("  {}", c.dim(&format!("— {l}")));
        }
    }
    let retried = if o.retried { format!("  {}", c.dim("(retried)")) } else { String::new() };
    println!(
        "  {:<34} {:<name_w$}  {}{tail}{retried}",
        fmt_verdict(c, &o.verdict),
        case.name,
        c.dim(&format!("{:6.2}s", case.wall))
    );
}

/// What each axis an alarm rests on said, in the order `by` names them: `qed: proved, fuzz:
/// counterexample`.
fn alarm_words(case: &Case, by: &[String]) -> String {
    let said = crate::pinned::observe(case, &crate::suite::AXES);
    let words: Vec<String> = by.iter().filter_map(|a| said.get(a).map(|(w, _)| format!("{a}: {w}"))).collect();
    words.join(", ")
}

/// An alarm outside `--portfolio`: case `case` (an index into the run's cases), on which the axes
/// `by` claimed equivalence and refuted it. `known` when `--expect pinned` finds the wrong side
/// pinned `!known-unsound`, still reproducing: a known bug, which passes as its pin does.
pub struct Alarm {
    pub case: usize,
    pub by: Vec<String>,
    pub known: bool,
}

/// The alarms of a run without `--portfolio`, whose own table lists its alarms.
pub fn print_alarms(c: Color, cases: &[Case], alarms: &[Alarm]) {
    if alarms.is_empty() {
        return;
    }
    println!();
    println!(
        "{}{}",
        c.red(&c.bold("  Alarms")),
        c.dim("  — a proof and a counterexample on one pair: one of those backends is wrong")
    );
    println!("{}", rule(c));
    for a in alarms {
        let x = &cases[a.case];
        let line = format!("  {}  {}  — {}", fmt_verdict(Color { on: false }, portfolio::ALARM), x.name, alarm_words(x, &a.by));
        if a.known {
            println!("{}", c.dim(&format!("{line}  (pinned !known-unsound, still reproducing)")));
        } else {
            println!("{}", c.red(&c.bold(&line)));
        }
    }
    if alarms.iter().any(|a| !a.known) {
        println!("{}", c.dim("  note  an alarm fails the run whatever --expect says."));
    }
}

/// Seconds as the user wrote them: `60`, `0.5`.
pub fn fmt_secs(x: f64) -> String {
    if x.fract() == 0.0 {
        format!("{x:.0}")
    } else {
        format!("{x}")
    }
}

fn median(mut xs: Vec<f64>) -> Option<f64> {
    if xs.is_empty() {
        return None;
    }
    xs.sort_by(f64::total_cmp);
    let n = xs.len();
    Some(if n % 2 == 1 { xs[n / 2] } else { (xs[n / 2 - 1] + xs[n / 2]) / 2.0 })
}

/// The combined verdicts, and what they rest on.
pub fn print_portfolio(c: Color, cases: &[Case], backends: &[&str], deadline: f64, retried: usize) {
    let outcomes: Vec<(&Case, &portfolio::Outcome)> =
        cases.iter().filter_map(|x| x.portfolio.as_ref().map(|o| (x, o))).collect();
    if outcomes.is_empty() {
        return;
    }
    let count = |v: &str| outcomes.iter().filter(|(_, o)| o.verdict == v).count();
    println!();
    println!(
        "{}{}",
        c.bold("  Portfolio"),
        c.dim(&format!("  — {} on each case at once, {}s deadline", backends.join(", "), fmt_secs(deadline)))
    );
    println!("{}", rule(c));
    for v in portfolio::ORDER {
        let n = count(v);
        if n > 0 {
            println!("  {:<32} {n:>5}", fmt_verdict(c, v));
        }
    }
    println!("{}", rule(c));
    // The qed axis's `capability` footing, pairs whose two queries differ, less the refused pairs
    // the frontend found reflexive: here those are `equivalent`, and their two sides are one query,
    // so counting them would turn a difference in text alone into capability.
    let differ: Vec<&portfolio::Outcome> = outcomes
        .iter()
        .filter(|(x, _)| x.trivial == Some(false) && !x.reflexive)
        .map(|(_, o)| *o)
        .collect();
    if !differ.is_empty() {
        let n = differ.iter().filter(|o| o.verdict == portfolio::EQUIVALENT).count();
        let pct = 100.0 * n as f64 / differ.len() as f64;
        println!(
            "  {:<13} {n}/{}  ({pct:.1}%)   {}",
            c.bold("capability"),
            differ.len(),
            c.dim("equivalent among pairs whose two queries differ")
        );
    }
    let eq: Vec<&portfolio::Outcome> =
        outcomes.iter().filter(|(_, o)| o.verdict == portfolio::EQUIVALENT).map(|(_, o)| *o).collect();
    if !eq.is_empty() {
        let has = |o: &portfolio::Outcome, a: &str| o.by.iter().any(|b| b == a);
        let n = |f: &dyn Fn(&portfolio::Outcome) -> bool| eq.iter().filter(|o| f(o)).count();
        // Only the provers that ran: a count for one that was not asked would read as one that
        // proved nothing.
        let mut parts = Vec::new();
        match (backends.contains(&"qed"), backends.contains(&"sqleq-solver")) {
            (true, true) => {
                let qed = n(&|o| has(o, "qed") && !has(o, "sqleq-solver"));
                let ss = n(&|o| has(o, "sqleq-solver") && !has(o, "qed"));
                let both = n(&|o| has(o, "qed") && has(o, "sqleq-solver"));
                parts.push(format!("qed alone {qed} · sqleq-solver alone {ss} · both {both}"));
            }
            (true, false) => parts.push(format!("qed {}", n(&|o| has(o, "qed")))),
            (false, true) => parts.push(format!("sqleq-solver {}", n(&|o| has(o, "sqleq-solver")))),
            (false, false) => {}
        }
        // Settled with no prover: the frontend found the two sides one query.
        let refl = n(&|o| !has(o, "qed") && !has(o, "sqleq-solver"));
        if refl > 0 {
            parts.push(format!("reflexivity alone {refl}"));
        }
        if !parts.is_empty() {
            println!("  {:<13} {}", c.dim("proved by"), parts.join(" · "));
        }
    }
    let first: Vec<f64> = outcomes.iter().filter_map(|(_, o)| o.first_s).collect();
    let walls: Vec<f64> = outcomes.iter().map(|(x, _)| x.wall).collect();
    if let (Some(m), Some(w)) = (median(first.clone()), median(walls.clone())) {
        let slowest = |xs: &[f64]| xs.iter().copied().fold(0.0, f64::max);
        println!(
            "  {:<13} median {m:.2}s · slowest {:.2}s   {}",
            c.dim("first answer"),
            slowest(&first),
            c.dim(&format!("(case wall: median {w:.2}s · slowest {:.2}s)", slowest(&walls)))
        );
    }
    let cut: Vec<String> = backends
        .iter()
        .chain(["frontend"].iter())
        .filter_map(|a| {
            let n = outcomes.iter().filter(|(_, o)| o.pending.iter().any(|p| p == a)).count();
            (n > 0).then(|| format!("{a} {n}"))
        })
        .collect();
    if !cut.is_empty() {
        println!("  {:<13} {}", c.dim("cut off"), c.yellow(&cut.join(" · ")));
    }
    if retried > 0 {
        let won = outcomes.iter().filter(|(_, o)| o.retried).count();
        println!("  {:<13} {retried} case(s) re-run serially, {won} decided by it", c.dim("retried"));
    }
    for (x, o) in outcomes.iter().filter(|(_, o)| o.verdict == portfolio::ALARM) {
        println!("{}", c.red(&format!("  ALARM  {}  — {}", x.name, alarm_words(x, &o.by))));
    }
    println!(
        "{}",
        c.dim(
            "  note  `equivalent` is a proof under index binding, the claim `provable` makes; the\n        \
             gather verdicts are the Lean axis's weaker claims. `timeout` and `undecided` say only\n        \
             that no backend decided the pair in time -- neither is `not-equivalent`."
        )
    );
}

/// Counts by (status, trivial). `capability` is the headline: how many pairs that actually differ
/// were proved.
#[derive(Clone, Debug, Default, Serialize)]
pub struct Triviality {
    pub trivial: usize,
    pub nontrivial: usize,
    pub undetermined: usize,
    pub by_text: usize,
    pub trivial_proved: usize,
    pub capability: usize,
    pub capability_of: usize,
}

pub fn triviality_split(cases: &[Case]) -> Triviality {
    let nontrivial: Vec<&Case> = cases.iter().filter(|x| x.trivial == Some(false)).collect();
    let trivial: Vec<&Case> = cases.iter().filter(|x| x.trivial == Some(true)).collect();
    Triviality {
        trivial: trivial.len(),
        nontrivial: nontrivial.len(),
        undetermined: cases.iter().filter(|x| x.trivial.is_none()).count(),
        by_text: cases.iter().filter(|x| x.trivial_basis == "text").count(),
        trivial_proved: trivial.iter().filter(|x| x.status == PROVABLE).count(),
        capability: nontrivial.iter().filter(|x| x.status == PROVABLE).count(),
        capability_of: nontrivial.len(),
    }
}

/// The proved/total line counts reflexive pairs, which the prover did not earn. Print what it did
/// earn, right underneath, so the two numbers are never seen apart.
fn print_capability(c: Color, cases: &[Case]) {
    let s = triviality_split(cases);
    if s.capability_of == 0 && s.trivial == 0 {
        return;
    }
    if s.capability_of > 0 {
        let pct = 100.0 * s.capability as f64 / s.capability_of as f64;
        println!(
            "  {:<13} {}/{}  ({pct:.1}%)   {}",
            c.bold("capability"),
            s.capability,
            s.capability_of,
            c.dim("pairs whose two queries differ")
        );
    } else {
        println!("  {:<13} n/a           {}", c.bold("capability"), c.dim("no pair here has two differing queries"));
    }
    let mut detail = format!("{}/{} of the reflexive (x vs x) pairs also proved", s.trivial_proved, s.trivial);
    if s.undetermined > 0 {
        detail += &format!("; {} undetermined", s.undetermined);
    }
    println!("{}", c.dim(&format!("  {:<13} {detail}", "")));
}

pub fn print_summary(c: Color, cases: &[Case], wall: f64, qed: bool) {
    let count = |s: &str| cases.iter().filter(|x| x.status == s).count();
    let total = cases.len();
    println!();
    println!("{}", c.bold("  Summary"));
    println!("{}", rule(c));
    for s in STATUS_ORDER {
        let n = count(s);
        if n == 0 {
            continue;
        }
        println!("  {:<22} {n:>5}", fmt_status(c, s));
        // Refusals are a soundness feature, not a bug -- break them down under their own row so
        // deliberate declines are distinguishable from parse gaps at a glance.
        if s == REFUSED {
            let mut kinds: BTreeMap<&str, usize> = BTreeMap::new();
            for x in cases.iter().filter(|x| x.status == REFUSED) {
                *kinds.entry(&x.refuse_kind).or_default() += 1;
            }
            let detail: Vec<String> = kinds.iter().map(|(k, n)| format!("{k} {n}")).collect();
            println!("{}", c.dim(&format!("  {:<24}{}", "", detail.join(", "))));
        }
    }
    println!("{}", rule(c));
    // Without the qed axis nothing was proved or left unproved, and a `proved 0/N` line would read
    // as a prover that failed everything.
    if qed {
        let provable = count(PROVABLE);
        let pct = if total > 0 { 100.0 * provable as f64 / total as f64 } else { 0.0 };
        println!("  {:<13} {provable}/{total}  ({pct:.1}%)", c.bold("proved"));
        print_capability(c, cases);
    }
    println!("  {:<13} {wall:.2}s", c.dim("wall time"));
    if !cases.is_empty() {
        let avg = cases.iter().map(|x| x.wall).sum::<f64>() / cases.len() as f64;
        // The first of the slowest, as Python's `max` picks it.
        let slowest = cases.iter().fold(&cases[0], |m, x| if x.wall > m.wall { x } else { m });
        println!(
            "  {:<13} {avg:.2}s   {} {} ({:.2}s)",
            c.dim("avg / case"),
            c.dim("slowest"),
            slowest.name,
            slowest.wall
        );
    }
}

/// The SQLSolver axis, and the warnings that have to travel with it. With the qed axis asked (`qed`)
/// it is a second opinion, set against the QED prover's proofs on the same pairs; without it there
/// is nothing to set it against, and its proofs are counted on their own.
pub fn print_second_opinion(c: Color, cases: &[Case], stats: &solver::Stats, imp: &str, qed: bool) {
    let who = solver::name(imp);
    let scored: Vec<&Case> = cases.iter().filter(|x| x.s_bucket.is_some()).collect();
    if scored.is_empty() {
        return;
    }
    let mut counts: HashMap<&str, usize> = HashMap::new();
    for x in &scored {
        *counts.entry(x.s_bucket.as_deref().unwrap_or("")).or_default() += 1;
    }
    println!();
    if qed {
        println!("{}{}", c.bold("  Second opinion"), c.dim(&format!("  — {who}, over the same lowered IR")));
    } else {
        println!("{}{}", c.bold("  SQLSolver axis"), c.dim(&format!("  — {who}, over the lowered IR")));
    }
    println!("{}", rule(c));
    for b in solver::ORDER {
        if let Some(n) = counts.get(b).filter(|n| **n > 0) {
            println!("  {b:<22} {n:>5}");
        }
    }
    println!("{}", rule(c));

    // These cells stand on the same footing as `capability`: pairs whose two queries actually
    // differ, and real proofs only. A pair that reaches either prover as `x` against `x` was
    // answered by neither -- their `proved-literal` is this harness's `trivial` seen from the
    // other side -- so counting it would inflate both columns and the agreement between them.
    let diff: Vec<&&Case> = scored.iter().filter(|x| x.trivial == Some(false)).collect();
    if qed {
        let ours: BTreeSet<&str> = diff.iter().filter(|x| x.status == PROVABLE).map(|x| x.name.as_str()).collect();
        let theirs: BTreeSet<&str> =
            diff.iter().filter(|x| x.s_bucket.as_deref() == Some(solver::PROVED)).map(|x| x.name.as_str()).collect();
        let both = ours.intersection(&theirs).count();
        let only_p = ours.difference(&theirs).count();
        let only_s: Vec<&str> = theirs.difference(&ours).copied().collect();
        let union = ours.union(&theirs).count();
        println!("  {:<22} {:>5}", "of pairs that differ", diff.len());
        println!("  {:<22} {both:>5}", "both provers");
        println!("  {:<22} {only_p:>5}", "only the QED prover");
        println!(
            "  {:<22} {}   {}",
            format!("only {who}"),
            c.bold(&format!("{:>5}", only_s.len())),
            c.dim("what the second opinion adds")
        );
        for name in only_s.iter().take(10) {
            println!("{}", c.dim(&format!("  {:<22}       {name}", "")));
        }
        if only_s.len() > 10 {
            println!("{}", c.dim(&format!("  {:<22}       … and {} more", "", only_s.len() - 10)));
        }
        println!("  {:<22} {:>5}", "neither", diff.len() - union);
    } else if diff.is_empty() {
        println!("  {:<13} n/a           {}", c.bold("proved"), c.dim("no pair here has two differing queries"));
    } else {
        let n = diff.iter().filter(|x| x.s_bucket.as_deref() == Some(solver::PROVED)).count();
        let pct = 100.0 * n as f64 / diff.len() as f64;
        println!(
            "  {:<13} {n}/{}  ({pct:.1}%)   {}",
            c.bold("proved"),
            diff.len(),
            c.dim("pairs whose two queries differ")
        );
    }
    if let Some(w) = stats.wall_s {
        let mut detail = format!("{w:.2}s over {} row(s)", stats.answered.unwrap_or(0));
        if stats.halts > 0 {
            detail += &format!(", {} driver self-halt(s) in {} pass(es)", stats.halts, stats.passes);
        }
        println!("{}", c.dim(&format!("  {:<22} {detail}", "wall time")));
    }
    if let Some(st) = &stats.stalled {
        let reason = if st.reason.is_empty() { "no message" } else { &st.reason };
        println!("{}", c.red(&format!("  stalled: IrDriver exited {} without answering a row — {reason}", st.exit)));
    }
    println!(
        "{}",
        c.dim(
            "  note  `no-proof` is not a refutation. That prover's NEQ means \"no proof\n        found\", exactly \
             like its UNKNOWN; only its EQ is a claim, and\n        sqleq-fuzz remains the only disprover here."
        )
    );
    if qed {
        println!(
            "{}",
            c.dim(
                "        Both opinions come through this repo's frontend, so where they\n        agree they \
                 corroborate the provers, not the lowering."
            )
        );
    }
}

pub fn print_fuzz(c: Color, cases: &[Case], stats: &fuzz::Stats) {
    let scored: Vec<&Case> = cases.iter().filter(|x| x.f_verdict.is_some()).collect();
    if scored.is_empty() {
        return;
    }
    let mut counts: BTreeMap<&str, usize> = BTreeMap::new();
    for x in &scored {
        *counts.entry(x.f_verdict.as_deref().unwrap_or("")).or_default() += 1;
    }
    println!();
    println!("{}{}", c.bold("  Fuzz axis"), c.dim("  — sqleq-fuzz, random instances in PostgreSQL"));
    println!("{}", rule(c));
    for (v, n) in &counts {
        println!("  {v:<22} {n:>5}");
    }
    println!("{}", rule(c));
    for x in scored.iter().filter(|x| x.f_verdict.as_deref() == Some("counterexample")) {
        println!("{}", c.dim(&format!("  {:<22}       {}", "counterexample", x.name)));
    }
    if let Some(w) = stats.wall_s {
        println!("{}", c.dim(&format!("  {:<22} {w:.2}s", "wall time")));
    }
    println!(
        "{}",
        c.dim(&format!(
            "  note  `no-counterexample` is not a proof: it is the verdict of {} trials\n        over a small value \
             domain (see sqleq-fuzz/README.md).",
            fuzz::FUZZ_ARGS[1]
        ))
    );
}

pub fn print_lean(c: Color, cases: &[Case], stats: &lean::Stats) {
    let scored: Vec<&Case> = cases.iter().filter(|x| x.l_verdict.is_some()).collect();
    if scored.is_empty() {
        return;
    }
    let mut counts: BTreeMap<&str, usize> = BTreeMap::new();
    for x in &scored {
        *counts.entry(x.l_verdict.as_deref().unwrap_or("")).or_default() += 1;
    }
    println!();
    println!("{}{}", c.bold("  Lean axis"), c.dim("  — INSERT … VALUES vs INSERT … SELECT * FROM unnest(…)"));
    println!("{}", rule(c));
    let extra = counts.keys().filter(|v| !lean::LEAN_ORDER.contains(v)).copied();
    for v in lean::LEAN_ORDER.iter().copied().chain(extra) {
        if let Some(n) = counts.get(v) {
            println!("  {v:<22} {n:>5}");
        }
    }
    println!("{}", rule(c));
    for x in &scored {
        if let Some(v) = x.l_verdict.as_deref().filter(|v| lean::LEAN_PROVED.contains(v)) {
            println!("{}", c.dim(&format!("  {v:<22}       {}", x.name)));
        }
    }
    if let Some(w) = stats.wall_s {
        println!("{}", c.dim(&format!("  {:<22} {w:.2}s", "wall time")));
    }
    println!(
        "{}",
        c.dim(
            "  note  `proved-gather` is proved under the gather rule: the unnest\n        side's array $j is column \
             j of the VALUES rows. It is not the\n        same-$N claim `provable` makes. \
             `proved-gather-generated` is\n        weaker still: the arrays also carry the values the VALUES \
             side's\n        generated cells (DEFAULT, now()) evaluated to. See docs/LEAN.md."
        )
    );
}

fn ljust(s: &str, w: usize) -> String {
    format!("{s:<w$}")
}

pub fn print_pinned(c: Color, pinned: &[Pinned], cases: &[Case], axes: &[&str]) {
    let cols: Vec<&str> = suite::AXES.iter().copied().filter(|a| axes.contains(a)).collect();
    let name_of = |p: &Pinned| cases[p.case].name.as_str();
    let name_w = pinned.iter().map(|p| name_of(p).chars().count()).max().unwrap_or(10).min(64);
    let truth = |t: Option<&str>| match t {
        Some(suite::EQUIVALENT) => "EQ",
        Some(suite::NOT_EQUIVALENT) => "NEQ",
        _ => "?",
    };
    let grid: Vec<Vec<String>> = pinned
        .iter()
        .map(|p| {
            let by: HashMap<&str, &Judgement> = p.judgements.iter().map(|j| (j.axis.as_str(), j)).collect();
            let mut row = vec![name_of(p).to_string(), truth(p.header.truth.as_deref()).to_string()];
            if p.lint.is_empty() {
                row.extend(cols.iter().map(|a| suite::cell(by.get(a).copied())));
            } else {
                row.extend(cols.iter().map(|_| "lint".to_string()));
            }
            row
        })
        .collect();
    let mut widths = vec![name_w, 5];
    for (i, a) in cols.iter().enumerate() {
        let w = grid.iter().map(|r| r[2 + i].chars().count()).chain([a.chars().count()]).max().unwrap_or(0);
        widths.push(w);
    }
    let heads: Vec<&str> = ["case", "truth"].iter().copied().chain(cols.iter().copied()).collect();
    let line_w: usize = widths.iter().sum::<usize>() + 2 * widths.len();
    println!();
    let head: Vec<String> = heads.iter().zip(&widths).map(|(h, w)| ljust(h, *w)).collect();
    println!("{}", c.bold(&format!("  {}", head.join("  "))));
    println!("{}", c.dim(&format!("  {}", "─".repeat(line_w))));
    for (row, p) in grid.iter().zip(pinned) {
        let cells: Vec<String> = row.iter().zip(&widths).map(|(v, w)| ljust(v, *w)).collect();
        let line = cells.join("  ");
        println!("  {}", if p.passed() { line } else { c.red(&line) });
    }
    let failed: Vec<&Pinned> = pinned.iter().filter(|p| !p.passed()).collect();
    println!("{}", c.dim(&format!("  {}", "─".repeat(line_w))));
    println!(
        "{}",
        c.dim("  ✓ pin holds  ≈ known-unsound, still reproducing  ✗ moved  + unpinned  ‼ contradicts truth  ⏱ no answer  · axis not run")
    );
    if !failed.is_empty() {
        println!();
        for p in &failed {
            println!("{}", c.bold(&format!("  {}", name_of(p))));
            for e in &p.lint {
                println!("{}", c.red(&format!("    lint: {e}")));
            }
            for j in p.judgements.iter().filter(|j| !j.passed()) {
                println!("{}", c.red(&format!("    {}", suite::explain(j, p.header.truth.as_deref()))));
                if !j.note.is_empty() {
                    println!("{}", c.dim(&format!("      {}", j.note)));
                }
            }
        }
    }
    println!();
    println!(
        "  {} {}/{} case(s) hold on {}",
        c.bold("pinned"),
        pinned.len() - failed.len(),
        pinned.len(),
        cols.join(", ")
    );
}

// --- --json -------------------------------------------------------------------------------------

#[derive(Serialize)]
pub struct SsMeta {
    #[serde(flatten)]
    pub stats: solver::Stats,
    #[serde(rename = "impl")]
    pub imp: String,
    #[serde(rename = "where")]
    pub location: String,
    pub timeout_ms: u64,
}

#[derive(Serialize)]
pub struct FuzzMeta {
    #[serde(flatten)]
    pub stats: fuzz::Stats,
    pub bin: String,
    pub args: Vec<String>,
}

#[derive(Serialize)]
pub struct LeanMeta {
    #[serde(flatten)]
    pub stats: lean::Stats,
    pub bin: String,
}

#[derive(Default, Serialize)]
pub struct VerdictCounts {
    pub alarm: usize,
    #[serde(rename = "not-equivalent")]
    pub not_equivalent: usize,
    pub equivalent: usize,
    #[serde(rename = "equivalent-gather")]
    pub equivalent_gather: usize,
    #[serde(rename = "equivalent-gather-generated")]
    pub equivalent_gather_generated: usize,
    pub timeout: usize,
    pub undecided: usize,
}

#[derive(Serialize)]
pub struct PortfolioMeta {
    pub deadline_s: f64,
    pub backends: Vec<String>,
    pub counts: VerdictCounts,
    pub alarms: Vec<String>,
    pub retried: usize,
}

impl PortfolioMeta {
    pub fn of(cases: &[Case], backends: &[&str], deadline_s: f64, retried: usize) -> PortfolioMeta {
        let mut counts = VerdictCounts::default();
        let mut alarms = Vec::new();
        for x in cases {
            let Some(o) = &x.portfolio else { continue };
            match o.verdict.as_str() {
                portfolio::ALARM => {
                    counts.alarm += 1;
                    alarms.push(x.name.clone());
                }
                portfolio::NOT_EQUIVALENT => counts.not_equivalent += 1,
                portfolio::EQUIVALENT => counts.equivalent += 1,
                portfolio::EQUIVALENT_GATHER => counts.equivalent_gather += 1,
                portfolio::EQUIVALENT_GATHER_GENERATED => counts.equivalent_gather_generated += 1,
                portfolio::TIMEOUT => counts.timeout += 1,
                _ => counts.undecided += 1,
            }
        }
        PortfolioMeta { deadline_s, backends: backends.iter().map(|b| b.to_string()).collect(), counts, alarms, retried }
    }
}

#[derive(Serialize)]
pub struct PinnedMeta {
    pub held: usize,
    pub total: usize,
    pub blessed: Vec<String>,
}

#[derive(Serialize)]
#[serde(untagged)]
pub enum Finding {
    Lint { case: String, lint: String },
    Moved { case: String, axis: String, state: String, observed: String, pinned: Option<String>, note: String },
}

#[derive(Serialize)]
pub struct Meta {
    pub axes: Vec<String>,
    pub frontend: Option<String>,
    pub prover: Option<String>,
    pub jobs: usize,
    pub timeout_s: f64,
    pub smt_timeout_ms: Option<u64>,
    pub total: usize,
    pub wall_s: f64,
    pub triviality: Triviality,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sqlsolver: Option<SsMeta>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fuzz: Option<FuzzMeta>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lean: Option<LeanMeta>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub portfolio: Option<PortfolioMeta>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pinned: Option<PinnedMeta>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub findings: Option<Vec<Finding>>,
    /// The cases on which a proof and a counterexample met, and which fail the run for it: every
    /// alarm but one `--expect pinned` finds pinned `!known-unsound`.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub alarms: Vec<String>,
}

#[derive(Serialize)]
struct Payload<'a> {
    meta: &'a Meta,
    cases: &'a [Case],
}

pub fn write_json(path: &str, cases: &[Case], meta: &Meta) -> std::io::Result<()> {
    let text = serde_json::to_string_pretty(&Payload { meta, cases }).map_err(std::io::Error::other)?;
    std::fs::write(path, text)
}

pub fn write_csv(path: &str, cases: &[Case], portfolio: bool) -> std::io::Result<()> {
    let mut cols = vec![
        "name",
        "status",
        "trivial",
        "trivial_basis",
        "refuse_kind",
        "wall",
        "lower_wall",
        "prove_wall",
        "complete_fragment",
        "smt_timed_out",
        "nontrivial_perms",
        "message",
        "s_bucket",
        "s_verdict",
        "s_ms",
        "s_note",
        "l_verdict",
        "l_reason",
        "l_shape",
        "l_ms",
        "f_verdict",
        "f_note",
        "f_ms",
    ];
    // Appended, so a reader of the batch columns finds them where they always were.
    if portfolio {
        cols.extend(["p_verdict", "p_by", "p_pending", "p_first_s", "p_retried"]);
    }
    let mut w = csv::WriterBuilder::new().terminator(csv::Terminator::CRLF).from_path(path)?;
    w.write_record(&cols)?;
    let opt = |v: &Option<String>| v.clone().unwrap_or_default();
    let num = |v: &Option<serde_json::Value>| v.as_ref().map(|v| v.to_string()).unwrap_or_default();
    for x in cases {
        let mut row = vec![
            x.name.clone(),
            x.status.clone(),
            x.trivial.map(|t| t.to_string()).unwrap_or_default(),
            x.trivial_basis.clone(),
            x.refuse_kind.clone(),
            x.wall.to_string(),
            x.lower_wall.to_string(),
            x.prove_wall.to_string(),
            x.complete_fragment.to_string(),
            x.smt_timed_out.to_string(),
            x.nontrivial_perms.to_string(),
            x.message.clone(),
            opt(&x.s_bucket),
            opt(&x.s_verdict),
            num(&x.s_ms),
            x.s_note.clone(),
            opt(&x.l_verdict),
            x.l_reason.clone(),
            x.l_shape.clone(),
            num(&x.l_ms),
            opt(&x.f_verdict),
            x.f_note.clone(),
            x.f_ms.map(|m| m.to_string()).unwrap_or_default(),
        ];
        if portfolio {
            let o = x.portfolio.clone().unwrap_or_default();
            row.extend([
                o.verdict,
                o.by.join(";"),
                o.pending.join(";"),
                o.first_s.map(|f| f.to_string()).unwrap_or_default(),
                o.retried.to_string(),
            ]);
        }
        w.write_record(&row)?;
    }
    w.flush()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::case::ERROR;

    #[test]
    fn statuses_carry_their_glyphs() {
        let c = Color { on: false };
        assert_eq!(fmt_status(c, PROVABLE), "✓ provable");
        assert_eq!(fmt_status(c, UNPROVABLE), "✗ unprovable");
        assert_eq!(fmt_status(c, TIMEOUT), "⏱ timeout");
        assert_eq!(fmt_status(c, REFUSED), "⚠ refused");
        assert_eq!(fmt_status(c, PANIC), "💥 panic");
        assert_eq!(fmt_status(c, ERROR), "? error");
        assert_eq!(fmt_status(c, LOWERED), "· lowered");
    }
}
