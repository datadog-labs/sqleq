// Unless explicitly stated otherwise all files in this repository are licensed under the
// Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/).
// Copyright 2026-Present Datadog, Inc.

//! Rung 2½: congruence of two terms under bag semantics, with Z3 deciding their 0/1 parts.
//!
//! Rung 2 compares two normalized terms up to renaming, and rung 3 hands Z3 only terms whose every
//! sum sits under a squash. A query without `DISTINCT` has a sum at the top, so two such queries
//! that say the same thing in two ways -- `x IS NULL OR x IS NULL` and `x IS NULL`, an `IN` list a
//! disjunction already implies, an `UPDATE` that skips rows whose value would not change -- reach
//! Z3 nowhere. This rung matches the two terms' structure and asks Z3 only about the parts a
//! solver can decide:
//!
//! * **Two sums** match when some bijection of their bound vars (same widths, guarded by the same
//!   tables) makes their bodies match. Under it the two bodies speak about the same rows.
//! * **Two sums of terms** match when their summands match one to one.
//! * **Two products** match when their factors that are not 0/1 (table atoms, nested sums, values)
//!   match one to one, and their 0/1 factors (predicates, squashes, negations) agree wherever the
//!   others are non-zero: Z3 proves `ctx ⇒ (∧B_A ⇔ ∧B_B)`, where `ctx` says each matched table atom
//!   holds its row.
//!
//! Matched products are equal at every assignment of their vars: where the shared factors are zero
//! both are, and elsewhere their 0/1 parts are equal. Matched sums of pointwise-equal bodies are
//! equal, and so are matched sums of terms. The claim is pointwise, and nothing it cannot pair is
//! proved: a different number of tables, a predicate that changes how often a row is counted (a
//! join without a key), or a value only one side reads leaves the pair to the rungs above.
//!
//! Inside a 0/1 part, a value-position sum (an aggregate, a scalar subquery) is paired with one on
//! the other side by the same matching, which proves the two equal at every assignment of the
//! columns they read, and a pair is then one uninterpreted function of those columns. The encoding
//! also reads `CASE`, `COALESCE` and NULL sentinels as selections, and gives every row the facts
//! its table promises ([`crate::smt`]).

use std::collections::HashMap;
use std::rc::Rc;
use std::time::{Duration, Instant};

use z3::ast::Bool;
use z3::{Params, SatResult, Solver};

use crate::normalize::is_zero_one;
use crate::setsolver::TIMEOUT_MS;
use crate::smt::{free_columns, Encoder, Facts, Side, ValueSums};
use crate::translate::OUT_VAR_ID;
use crate::uterm::{UTerm, UVar};

/// Z3 calls one pair may make. Running out answers "not proved", which is sound.
const MAX_CALLS: usize = 64;
/// Bijections tried for one pair of sums.
const MAX_BIJECTIONS: usize = 24;
/// Matching steps for one pair.
const MAX_STEPS: usize = 200_000;
/// How long after its ladder starts a pair may still be in this rung, both attempts together. The
/// proofs it finds take well under a second; a pair whose normalization alone runs past this is
/// left to the rungs above.
pub const BUDGET: Duration = Duration::from_secs(5);
/// Terms larger than this (as trees) are not encoded: the encoding walks shared subterms once per
/// occurrence.
const MAX_TREE: usize = 100_000;
/// The right side's bound vars are moved past this, so the two sides' ids never meet.
const OFFSET: u32 = 1 << 30;

/// Whether the two normalized terms are equal on every database, as the matching above shows.
/// `false` is "not proved", never a disproof. `facts` are the schemas' row facts; past `deadline`
/// the rung gives up.
pub fn prove(
    a: &UTerm,
    widths_a: &HashMap<u32, usize>,
    b: &UTerm,
    widths_b: &HashMap<u32, usize>,
    facts: &Facts,
    deadline: Instant,
) -> bool {
    let bound = |w: &HashMap<u32, usize>| w.keys().all(|id| *id == OUT_VAR_ID || *id < OFFSET);
    if !bound(widths_a) || !bound(widths_b) || widths_a.get(&OUT_VAR_ID) != widths_b.get(&OUT_VAR_ID) {
        return false;
    }
    if Instant::now() >= deadline || a.tree_size(MAX_TREE + 1) > MAX_TREE || b.tree_size(MAX_TREE + 1) > MAX_TREE {
        return false;
    }
    let b = shift(b);
    let mut widths = widths_a.clone();
    widths.extend(widths_b.iter().filter(|(id, _)| **id != OUT_VAR_ID).map(|(id, w)| (id + OFFSET, *w)));
    Congruence { widths, facts, calls: 0, steps: 0, deadline }.congruent(a, &b)
}

/// `t` with every bound var moved past [`OFFSET`]; the output var stays.
fn shift(t: &UTerm) -> UTerm {
    type Memo = HashMap<*const UTerm, Rc<UTerm>>;
    fn var(v: &UVar) -> UVar {
        match v {
            UVar::Base(id) if *id == OUT_VAR_ID => v.clone(),
            UVar::Base(id) => UVar::Base(id + OFFSET),
            UVar::Proj { index, base } => UVar::Proj { index: *index, base: Box::new(var(base)) },
        }
    }
    fn rc(c: &Rc<UTerm>, memo: &mut Memo) -> Rc<UTerm> {
        if let Some(done) = memo.get(&Rc::as_ptr(c)) {
            return done.clone();
        }
        let done = Rc::new(go(c, memo));
        memo.insert(Rc::as_ptr(c), done.clone());
        done
    }
    fn go(t: &UTerm, memo: &mut Memo) -> UTerm {
        match t {
            UTerm::Const(_) => t.clone(),
            UTerm::Var(v) => UTerm::Var(var(v)),
            UTerm::Table { name, var: v } => UTerm::Table { name: name.clone(), var: var(v) },
            UTerm::Pred { kind, args } => UTerm::Pred { kind: *kind, args: args.iter().map(|a| go(a, memo)).collect() },
            UTerm::Func { name, args } => UTerm::Func { name: name.clone(), args: args.iter().map(|a| go(a, memo)).collect() },
            UTerm::Add(ts) => UTerm::Add(ts.iter().map(|c| rc(c, memo)).collect()),
            UTerm::Mul(ts) => UTerm::Mul(ts.iter().map(|c| rc(c, memo)).collect()),
            UTerm::Squash(c) => UTerm::Squash(rc(c, memo)),
            UTerm::Neg(c) => UTerm::Neg(rc(c, memo)),
            UTerm::Sum { vars, body } => UTerm::Sum { vars: vars.iter().map(var).collect(), body: rc(body, memo) },
        }
    }
    go(t, &mut Memo::new())
}

/// A product's factors; any other term is a product of one.
fn factors(t: &UTerm) -> Vec<UTerm> {
    match t {
        UTerm::Mul(fs) => fs.iter().map(|f| (**f).clone()).collect(),
        other => vec![other.clone()],
    }
}

/// Per bound var of a sum, the tables its body guards it by (a table atom over it, squashed or not,
/// among the body's factors): two vars can stand for the same rows only if these agree.
fn guards(ids: &[u32], body: &UTerm) -> HashMap<u32, Vec<String>> {
    let mut out: HashMap<u32, Vec<String>> = ids.iter().map(|id| (*id, Vec::new())).collect();
    for f in factors(body) {
        let atom = match &f {
            UTerm::Squash(c) => (**c).clone(),
            other => other.clone(),
        };
        if let UTerm::Table { name, var: UVar::Base(id) } = atom {
            if let Some(names) = out.get_mut(&id) {
                names.push(name);
            }
        }
    }
    out.values_mut().for_each(|names| names.sort());
    out
}

/// The sums `ts` read in value positions (aggregates, scalar subqueries), not those inside them.
fn value_sums(ts: &[UTerm]) -> Vec<UTerm> {
    fn go(t: &UTerm, value: bool, out: &mut Vec<UTerm>) {
        match t {
            UTerm::Sum { .. } if value => {
                if !out.contains(t) {
                    out.push(t.clone());
                }
            }
            UTerm::Sum { body, .. } => go(body, false, out),
            UTerm::Pred { args, .. } | UTerm::Func { args, .. } => args.iter().for_each(|a| go(a, true, out)),
            UTerm::Add(cs) | UTerm::Mul(cs) => cs.iter().for_each(|c| go(c, value, out)),
            UTerm::Squash(c) | UTerm::Neg(c) => go(c, false, out),
            UTerm::Const(_) | UTerm::Var(_) | UTerm::Table { .. } => {}
        }
    }
    let mut out = Vec::new();
    ts.iter().for_each(|t| go(t, false, &mut out));
    out
}

struct Congruence<'f> {
    widths: HashMap<u32, usize>,
    facts: &'f Facts,
    calls: usize,
    steps: usize,
    deadline: Instant,
}

impl Congruence<'_> {
    fn spent(&self) -> bool {
        self.steps > MAX_STEPS || self.calls >= MAX_CALLS || Instant::now() >= self.deadline
    }

    fn congruent(&mut self, a: &UTerm, b: &UTerm) -> bool {
        self.steps += 1;
        if self.spent() {
            return false;
        }
        if a == b {
            return true;
        }
        match (a, b) {
            (UTerm::Sum { vars: va, body: ba }, UTerm::Sum { vars: vb, body: bb }) => self.sum(va, ba, vb, bb),
            (UTerm::Add(xs), UTerm::Add(ys)) => self.summands(xs, ys),
            _ => self.product(&factors(a), &factors(b)),
        }
    }

    /// One to one, greedily: each summand of `xs` takes the first unused summand of `ys` it matches.
    /// Greedy can miss a matching that exists, never invent one.
    fn summands(&mut self, xs: &[Rc<UTerm>], ys: &[Rc<UTerm>]) -> bool {
        if xs.len() != ys.len() {
            return false;
        }
        let mut used = vec![false; ys.len()];
        for x in xs {
            let Some(k) = (0..ys.len()).find(|&k| !used[k] && self.congruent(x, &ys[k])) else { return false };
            used[k] = true;
        }
        true
    }

    fn product(&mut self, xs: &[UTerm], ys: &[UTerm]) -> bool {
        let (bx, mx): (Vec<UTerm>, Vec<UTerm>) = xs.iter().cloned().partition(is_zero_one);
        let (by, my): (Vec<UTerm>, Vec<UTerm>) = ys.iter().cloned().partition(is_zero_one);
        if mx.len() != my.len() {
            return false;
        }
        let mut used = vec![false; my.len()];
        for m in &mx {
            let Some(k) = (0..my.len()).find(|&k| !used[k] && self.matches(m, &my[k])) else { return false };
            used[k] = true;
        }
        let ctx: Vec<UTerm> = mx.into_iter().filter(|m| matches!(m, UTerm::Table { .. })).collect();
        self.equivalent(&ctx, &bx, &by)
    }

    /// Two factors that are not 0/1: the same term, or two sums (of terms) that match.
    fn matches(&mut self, a: &UTerm, b: &UTerm) -> bool {
        if a == b {
            return true;
        }
        match (a, b) {
            (UTerm::Sum { vars: va, body: ba }, UTerm::Sum { vars: vb, body: bb }) => self.sum(va, ba, vb, bb),
            (UTerm::Add(xs), UTerm::Add(ys)) => self.summands(xs, ys),
            _ => false,
        }
    }

    fn sum(&mut self, va: &[UVar], ba: &UTerm, vb: &[UVar], bb: &UTerm) -> bool {
        let ids = |vs: &[UVar]| vs.iter().map(|v| if let UVar::Base(id) = v { Some(*id) } else { None }).collect::<Option<Vec<u32>>>();
        let (Some(xs), Some(ys)) = (ids(va), ids(vb)) else { return false };
        if xs.len() != ys.len() {
            return false;
        }
        let (ga, gb) = (guards(&xs, ba), guards(&ys, bb));
        let cands: Vec<Vec<u32>> = xs
            .iter()
            .map(|x| ys.iter().copied().filter(|y| self.widths.get(x) == self.widths.get(y) && ga.get(x) == gb.get(y)).collect())
            .collect();
        if cands.iter().any(Vec::is_empty) {
            return false;
        }
        let mut pick = vec![0; xs.len()];
        let mut tried = 0;
        self.assign(&xs, &cands, 0, &mut pick, &mut tried, ba, bb)
    }

    /// Tries the bijections `cands` allows, one var at a time; each complete one renames the right
    /// body's vars to the left's and compares the bodies.
    #[allow(clippy::too_many_arguments)]
    fn assign(&mut self, xs: &[u32], cands: &[Vec<u32>], k: usize, pick: &mut [u32], tried: &mut usize, ba: &UTerm, bb: &UTerm) -> bool {
        if k == xs.len() {
            *tried += 1;
            let renamed = xs.iter().zip(pick.iter()).fold(bb.clone(), |t, (x, y)| t.rename_base(*y, *x));
            return self.congruent(ba, &renamed);
        }
        for &y in &cands[k] {
            if pick[..k].contains(&y) {
                continue;
            }
            pick[k] = y;
            if self.assign(xs, cands, k + 1, pick, tried, ba, bb) {
                return true;
            }
            if *tried >= MAX_BIJECTIONS || self.spent() {
                return false;
            }
        }
        false
    }

    /// Pairs the value-position sums of the two sides, each left one with the first unused right
    /// one that reads the same columns and matches it; every sum gets a class, a pair one class.
    /// Groups the value-position sums of both sides into classes: each joins the first class whose
    /// representative it matches and reads the same columns as, or starts one. A class is one
    /// function of those columns, every member equal to its representative. Copies of one sum on one
    /// side share a class too, which a selection needs: `CASE WHEN S IS NULL … ELSE f(S)` reads `S`
    /// twice, and its guards exclude each other only if both reads are one value.
    fn pair_value_sums(&mut self, bx: &[UTerm], by: &[UTerm]) -> ValueSums {
        let mut sums = value_sums(bx);
        for s in value_sums(by) {
            if !sums.contains(&s) {
                sums.push(s);
            }
        }
        let mut classes: Vec<(UTerm, String, Vec<UTerm>)> = Vec::new();
        let mut out = ValueSums::new();
        for s in sums {
            let reads = free_columns(&s, &self.widths);
            let class = classes
                .iter()
                .find(|(rep, _, r)| *r == reads && self.congruent(rep, &s))
                .map(|(_, name, _)| name.clone());
            let name = class.unwrap_or_else(|| {
                let name = format!("s{}", classes.len());
                classes.push((s.clone(), name.clone(), reads.clone()));
                name
            });
            out.insert(s, (name, reads));
        }
        out
    }

    /// Whether `ctx ⇒ (∧bx ⇔ ∧by)` holds, together with the obligations the encoding records.
    fn equivalent(&mut self, ctx: &[UTerm], bx: &[UTerm], by: &[UTerm]) -> bool {
        let sums = self.pair_value_sums(bx, by);
        if self.spent() {
            return false;
        }
        self.calls += 1;
        let mut enc = Encoder::new().interpreting().with_facts(self.facts.clone()).with_value_sums(sums);
        // One namespace: the right side's vars were renamed apart, or to the left's they match.
        let side = Side::A;
        let all = |ts: &[UTerm], enc: &mut Encoder| -> Option<Bool> {
            let parts: Vec<Bool> = ts.iter().map(|t| enc.set(t, side, &self.widths)).collect::<Option<_>>()?;
            Some(if parts.is_empty() { Bool::from_bool(true) } else { Bool::and(&parts) })
        };
        let (Some(context), Some(a), Some(b)) = (all(ctx, &mut enc), all(bx, &mut enc), all(by, &mut enc)) else { return false };
        let mut claim = enc.obligations();
        claim.push(a.eq(&b));
        let left = self.deadline.saturating_duration_since(Instant::now()).as_millis().min(TIMEOUT_MS as u128) as u32;
        if left == 0 {
            return false;
        }
        let solver = Solver::new();
        let mut params = Params::new();
        params.set_u32("timeout", left);
        params.set_u32("random_seed", 9876);
        solver.set_params(&params);
        for fact in enc.background() {
            solver.assert(&fact);
        }
        solver.assert(&context);
        solver.assert(Bool::and(&claim).not());
        solver.check() == SatResult::Unsat
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::uterm::{PredKind, UConst};

    fn col(i: u32, v: u32) -> UTerm {
        UTerm::Var(UVar::proj(i, UVar::Base(v)))
    }

    fn pred(kind: PredKind, a: UTerm, b: UTerm) -> UTerm {
        UTerm::Pred { kind, args: vec![a, b] }
    }

    fn mul(fs: Vec<UTerm>) -> UTerm {
        UTerm::Mul(fs.into_iter().map(Rc::new).collect())
    }

    fn sum(v: u32, body: UTerm) -> UTerm {
        UTerm::Sum { vars: vec![UVar::Base(v)], body: Rc::new(body) }
    }

    fn table(name: &str, v: u32) -> UTerm {
        UTerm::Table { name: name.into(), var: UVar::Base(v) }
    }

    fn null() -> UTerm {
        UTerm::Const(UConst::Null)
    }

    fn widths() -> HashMap<u32, usize> {
        HashMap::from([(0, 2), (OUT_VAR_ID, 1)])
    }

    fn soon() -> Instant {
        Instant::now() + BUDGET
    }

    /// `Σ_x t(x)·[out.0 = x.0]·cond`, a bag projection of `t(a, b)` filtered on `cond`.
    fn bag(cond: UTerm) -> UTerm {
        sum(0, mul(vec![table("t", 0), pred(PredKind::Eq, col(0, OUT_VAR_ID), col(0, 0)), cond]))
    }

    #[test]
    fn a_bag_sum_whose_condition_says_the_same_thing_is_proved() {
        // `WHERE b IS NULL OR b IS NULL` against `WHERE b IS NULL`.
        let is_null = || pred(PredKind::Eq, col(1, 0), null());
        let twice = UTerm::Squash(Rc::new(UTerm::Add(vec![Rc::new(is_null()), Rc::new(is_null())])));
        assert!(prove(&bag(twice), &widths(), &bag(is_null()), &widths(), &Facts::default(), soon()));
    }

    #[test]
    fn a_different_condition_is_not() {
        let one = UTerm::Const(UConst::Int(1));
        let lt = pred(PredKind::Lt, col(1, 0), one.clone());
        let le = UTerm::Neg(Rc::new(pred(PredKind::Lt, one, col(1, 0))));
        assert!(!prove(&bag(lt), &widths(), &bag(le), &widths(), &Facts::default(), soon()));
    }

    #[test]
    fn a_table_counted_once_is_not_a_table_counted_twice() {
        let cond = pred(PredKind::Eq, col(1, 0), null());
        let twice = sum(0, mul(vec![table("t", 0), table("t", 0), pred(PredKind::Eq, col(0, OUT_VAR_ID), col(0, 0)), cond.clone()]));
        assert!(!prove(&bag(cond), &widths(), &twice, &widths(), &Facts::default(), soon()));
    }
}
