// Unless explicitly stated otherwise all files in this repository are licensed under the
// Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/).
// Copyright 2026-Present Datadog, Inc.

//! Rung 2's decision: alpha-equivalence of two normalized terms (Java's `query1.equals(query2)`
//! under `UMulImpl.useWeakEquals`, in `SqlSolver.proveEq`).
//!
//! Two terms are alpha-equivalent when some bijection between their bound vars makes them equal,
//! with `Add` compared as a multiset, `Mul` as a multiset of its non-0/1 factors plus a set of its
//! 0/1 factors (`x·x = x` for those), and `Eq`/`Ne` arguments unordered. Every accepted bijection
//! is checked in full, so the search can only miss an equivalence, never invent one. Unlike Java,
//! the comparison is symmetric and `Add` is a true multiset: Java's `UAdd.equals` is a one-way
//! set comparison under which `a + a` equals `a + b`.

use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::rc::Rc;

use crate::normalize::is_zero_one;
use crate::uterm::{PredKind, UConst, UTerm, UVar};

/// Comparison steps before giving up. Giving up answers "not equivalent", which is sound.
const MAX_STEPS: usize = 500_000;

pub fn alpha_eq(a: &UTerm, widths_a: &HashMap<u32, usize>, b: &UTerm, widths_b: &HashMap<u32, usize>) -> bool {
    Matcher { widths_a, widths_b, ab: HashMap::new(), ba: HashMap::new(), steps: 0 }.eq(a, b)
}

struct Matcher<'w> {
    widths_a: &'w HashMap<u32, usize>,
    widths_b: &'w HashMap<u32, usize>,
    /// The bijection between the bound vars currently in scope on each side.
    ab: HashMap<u32, u32>,
    ba: HashMap<u32, u32>,
    steps: usize,
}

fn same_const(a: &UConst, b: &UConst) -> bool {
    let num = |c: &UConst| match c {
        UConst::Int(n) => Some(*n as f64),
        UConst::Decimal(s) => s.parse::<f64>().ok(),
        _ => None,
    };
    match (num(a), num(b)) {
        (Some(x), Some(y)) => x == y,
        _ => a == b,
    }
}

/// A hash that ignores every var id, so alpha-equivalent terms always agree on it. Used to skip
/// candidate pairs that cannot match before paying for a full comparison.
pub(crate) fn shape(t: &UTerm) -> u64 {
    fn go(t: &UTerm, h: &mut std::collections::hash_map::DefaultHasher) {
        std::mem::discriminant(t).hash(h);
        match t {
            UTerm::Const(c) => match c {
                UConst::Int(n) => n.hash(h),
                // Numeric constants are compared numerically, so hash only their kind.
                UConst::Decimal(_) => 0u8.hash(h),
                other => other.hash(h),
            },
            UTerm::Var(v) => proj_path(v, h),
            UTerm::Table { name, var } => {
                name.hash(h);
                proj_path(var, h);
            }
            UTerm::Pred { kind, args } => {
                kind.hash(h);
                // Eq/Ne arguments are unordered: combine them commutatively.
                let hs: Vec<u64> = args.iter().map(shape).collect();
                if matches!(kind, PredKind::Eq | PredKind::Ne) {
                    hs.iter().fold(0u64, |acc, x| acc.wrapping_add(*x)).hash(h);
                } else {
                    hs.hash(h);
                }
            }
            UTerm::Func { name, args } => {
                name.hash(h);
                args.iter().map(shape).collect::<Vec<_>>().hash(h);
            }
            UTerm::Add(ts) | UTerm::Mul(ts) => {
                let mut hs: Vec<u64> = ts.iter().map(|c| shape(c)).collect();
                hs.sort_unstable();
                hs.dedup();
                hs.hash(h);
            }
            UTerm::Squash(c) | UTerm::Neg(c) => go(c, h),
            UTerm::Sum { vars, body } => {
                vars.len().hash(h);
                go(body, h);
            }
        }
    }
    fn proj_path(v: &UVar, h: &mut std::collections::hash_map::DefaultHasher) {
        if let UVar::Proj { index, base } = v {
            index.hash(h);
            proj_path(base, h);
        }
    }
    let mut h = std::collections::hash_map::DefaultHasher::new();
    go(t, &mut h);
    h.finish()
}

/// Per bound var of a sum: the tables guarding it and how often it occurs -- a var-agnostic
/// fingerprint two matched vars must share.
fn signatures(vars: &[UVar], body: &UTerm) -> HashMap<u32, (Vec<String>, usize)> {
    fn base(v: &UVar) -> u32 {
        match v {
            UVar::Base(id) => *id,
            UVar::Proj { base: inner, .. } => base(inner),
        }
    }
    fn go(t: &UTerm, out: &mut HashMap<u32, (Vec<String>, usize)>) {
        match t {
            UTerm::Const(_) => {}
            UTerm::Var(v) => {
                if let Some(e) = out.get_mut(&base(v)) {
                    e.1 += 1;
                }
            }
            UTerm::Table { name, var } => {
                if let Some(e) = out.get_mut(&base(var)) {
                    e.0.push(name.clone());
                }
            }
            UTerm::Pred { args, .. } | UTerm::Func { args, .. } => args.iter().for_each(|a| go(a, out)),
            UTerm::Add(ts) | UTerm::Mul(ts) => ts.iter().for_each(|c| go(c, out)),
            UTerm::Squash(c) | UTerm::Neg(c) | UTerm::Sum { body: c, .. } => go(c, out),
        }
    }
    let mut out: HashMap<u32, (Vec<String>, usize)> =
        vars.iter().filter_map(|v| if let UVar::Base(id) = v { Some((*id, (Vec::new(), 0))) } else { None }).collect();
    go(body, &mut out);
    for e in out.values_mut() {
        e.0.sort();
    }
    out
}

impl Matcher<'_> {
    fn eq(&mut self, a: &UTerm, b: &UTerm) -> bool {
        self.steps += 1;
        if self.steps > MAX_STEPS {
            return false;
        }
        match (a, b) {
            (UTerm::Const(x), UTerm::Const(y)) => same_const(x, y),
            (UTerm::Var(x), UTerm::Var(y)) => self.var(x, y),
            (UTerm::Table { name: n1, var: v1 }, UTerm::Table { name: n2, var: v2 }) => n1 == n2 && self.var(v1, v2),
            (UTerm::Pred { kind: k1, args: a1 }, UTerm::Pred { kind: k2, args: a2 }) => {
                k1 == k2
                    && a1.len() == a2.len()
                    && (self.all(a1, a2)
                        || (matches!(k1, PredKind::Eq | PredKind::Ne)
                            && a1.len() == 2
                            && self.eq(&a1[0], &a2[1])
                            && self.eq(&a1[1], &a2[0])))
            }
            (UTerm::Func { name: n1, args: a1 }, UTerm::Func { name: n2, args: a2 }) => n1 == n2 && self.all(a1, a2),
            (UTerm::Squash(x), UTerm::Squash(y)) | (UTerm::Neg(x), UTerm::Neg(y)) => self.eq(x, y),
            (UTerm::Add(xs), UTerm::Add(ys)) => self.multiset(xs, ys),
            (UTerm::Mul(xs), UTerm::Mul(ys)) => self.product(xs, ys),
            (UTerm::Sum { vars: v1, body: b1 }, UTerm::Sum { vars: v2, body: b2 }) => self.sum(v1, b1, v2, b2),
            _ => false,
        }
    }

    fn all(&mut self, xs: &[UTerm], ys: &[UTerm]) -> bool {
        xs.len() == ys.len() && xs.iter().zip(ys).all(|(x, y)| self.eq(x, y))
    }

    fn var(&mut self, x: &UVar, y: &UVar) -> bool {
        match (x, y) {
            (UVar::Base(i), UVar::Base(j)) => match (self.ab.get(i), self.ba.get(j)) {
                (Some(m), _) => m == j,
                (None, Some(_)) => false,
                // Both free: the only free var is the shared output var.
                (None, None) => i == j,
            },
            (UVar::Proj { index: i, base: bx }, UVar::Proj { index: j, base: by }) => i == j && self.var(bx, by),
            _ => false,
        }
    }

    /// Multiset equality: with the bijection fixed, `eq` is an equivalence relation, so greedily
    /// pairing each term with any equal unused one decides it exactly.
    fn multiset(&mut self, xs: &[Rc<UTerm>], ys: &[Rc<UTerm>]) -> bool {
        if xs.len() != ys.len() {
            return false;
        }
        let ys_shape: Vec<u64> = ys.iter().map(|y| shape(y)).collect();
        let mut used = vec![false; ys.len()];
        'outer: for x in xs {
            let sx = shape(x);
            for (k, y) in ys.iter().enumerate() {
                if !used[k] && ys_shape[k] == sx && self.eq(x, y) {
                    used[k] = true;
                    continue 'outer;
                }
            }
            return false;
        }
        true
    }

    /// A product: its 0/1 factors as a set (duplicates collapsed under alpha-equivalence, since
    /// `x·x = x` for them), the rest as a multiset.
    fn product(&mut self, xs: &[Rc<UTerm>], ys: &[Rc<UTerm>]) -> bool {
        let (xi, xo): (Vec<Rc<UTerm>>, Vec<Rc<UTerm>>) = xs.iter().cloned().partition(|f| is_zero_one(f));
        let (yi, yo): (Vec<Rc<UTerm>>, Vec<Rc<UTerm>>) = ys.iter().cloned().partition(|f| is_zero_one(f));
        let xi = self.dedup_side(xi, true);
        let yi = self.dedup_side(yi, false);
        self.multiset(&xi, &yi) && self.multiset(&xo, &yo)
    }

    /// Collapses alpha-equivalent duplicates among one side's terms. The two copies share every
    /// bound var in scope, so those map to themselves.
    fn dedup_side(&mut self, ts: Vec<Rc<UTerm>>, side_a: bool) -> Vec<Rc<UTerm>> {
        if ts.len() < 2 {
            return ts;
        }
        let in_scope: Vec<u32> = if side_a { self.ab.keys().copied().collect() } else { self.ba.keys().copied().collect() };
        let identity: HashMap<u32, u32> = in_scope.into_iter().map(|v| (v, v)).collect();
        let widths = if side_a { self.widths_a } else { self.widths_b };
        let mut same = Matcher { widths_a: widths, widths_b: widths, ab: identity.clone(), ba: identity, steps: self.steps };
        let mut kept: Vec<Rc<UTerm>> = Vec::with_capacity(ts.len());
        for t in ts {
            let st = shape(&t);
            if !kept.iter().any(|k| shape(k) == st && same.eq(k, &t)) {
                kept.push(t);
            }
        }
        self.steps = same.steps;
        kept
    }

    fn sum(&mut self, v1: &[UVar], b1: &UTerm, v2: &[UVar], b2: &UTerm) -> bool {
        if v1.len() != v2.len() {
            return false;
        }
        let (s1, s2) = (signatures(v1, b1), signatures(v2, b2));
        let ids = |vs: &[UVar]| -> Vec<u32> { vs.iter().filter_map(|v| if let UVar::Base(id) = v { Some(*id) } else { None }).collect() };
        let (xs, ys) = (ids(v1), ids(v2));
        if xs.len() != v1.len() || ys.len() != v2.len() {
            return false;
        }
        // Candidates per var: same width, same fingerprint. Most constrained var first.
        let mut plan: Vec<(u32, Vec<u32>)> = xs
            .iter()
            .map(|x| {
                let cands = ys
                    .iter()
                    .copied()
                    .filter(|y| self.widths_a.get(x) == self.widths_b.get(y) && s1.get(x) == s2.get(y))
                    .collect();
                (*x, cands)
            })
            .collect();
        if plan.iter().any(|(_, c): &(u32, Vec<u32>)| c.is_empty()) {
            return false;
        }
        plan.sort_by_key(|(_, c)| c.len());
        self.assign(&plan, 0, b1, b2)
    }

    fn assign(&mut self, plan: &[(u32, Vec<u32>)], k: usize, b1: &UTerm, b2: &UTerm) -> bool {
        if k == plan.len() {
            return self.eq(b1, b2);
        }
        let (x, cands) = &plan[k];
        for y in cands {
            if self.ba.contains_key(y) {
                continue;
            }
            self.ab.insert(*x, *y);
            self.ba.insert(*y, *x);
            let ok = self.assign(plan, k + 1, b1, b2);
            self.ab.remove(x);
            self.ba.remove(y);
            if ok {
                return true;
            }
            if self.steps > MAX_STEPS {
                return false;
            }
        }
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::translate::OUT_VAR_ID;

    fn col(i: u32, v: u32) -> UTerm {
        UTerm::Var(UVar::proj(i, UVar::Base(v)))
    }

    fn eq(a: UTerm, b: UTerm) -> UTerm {
        UTerm::Pred { kind: PredKind::Eq, args: vec![a, b] }
    }

    fn table(name: &str, v: u32) -> UTerm {
        UTerm::Table { name: name.into(), var: UVar::Base(v) }
    }

    fn mul(fs: Vec<UTerm>) -> UTerm {
        UTerm::Mul(fs.into_iter().map(Rc::new).collect())
    }

    fn sum(vars: &[u32], body: UTerm) -> UTerm {
        UTerm::Sum { vars: vars.iter().map(|v| UVar::Base(*v)).collect(), body: Rc::new(body) }
    }

    fn widths(pairs: &[(u32, usize)]) -> HashMap<u32, usize> {
        pairs.iter().copied().collect()
    }

    #[test]
    fn bound_vars_match_under_a_bijection_and_factors_in_any_order() {
        // Σ_{0,1} R(0)·S(1)·[0.0 = 1.0]  vs  Σ_{7,3} [3.0 = 7.0]·S(3)·R(7)
        let a = sum(&[0, 1], mul(vec![table("r", 0), table("s", 1), eq(col(0, 0), col(0, 1))]));
        let b = sum(&[7, 3], mul(vec![eq(col(0, 3), col(0, 7)), table("s", 3), table("r", 7)]));
        let w = widths(&[(0, 1), (1, 1), (3, 1), (7, 1)]);
        assert!(alpha_eq(&a, &w, &b, &w));
        assert!(alpha_eq(&b, &w, &a, &w));
    }

    #[test]
    fn the_output_var_is_free_and_must_match_itself() {
        let a = mul(vec![table("r", OUT_VAR_ID)]);
        let b = sum(&[0], mul(vec![table("r", 0)]));
        let w = widths(&[(0, 1), (OUT_VAR_ID, 1)]);
        assert!(!alpha_eq(&a, &w, &b, &w));
    }

    #[test]
    fn add_is_a_true_multiset_unlike_javas_uadd() {
        let r = table("r", OUT_VAR_ID);
        let s = table("s", OUT_VAR_ID);
        let a = UTerm::Add(vec![Rc::new(r.clone()), Rc::new(r.clone())]);
        let b = UTerm::Add(vec![Rc::new(r), Rc::new(s)]);
        let w = widths(&[(OUT_VAR_ID, 1)]);
        assert!(!alpha_eq(&a, &w, &b, &w));
        assert!(!alpha_eq(&b, &w, &a, &w));
    }

    #[test]
    fn repeated_zero_one_factors_collapse_but_multiplicities_do_not() {
        let p = eq(col(0, OUT_VAR_ID), UTerm::Const(UConst::Int(1)));
        let r = table("r", OUT_VAR_ID);
        let w = widths(&[(OUT_VAR_ID, 1)]);
        assert!(alpha_eq(&mul(vec![p.clone(), p.clone(), r.clone()]), &w, &mul(vec![p.clone(), r.clone()]), &w));
        assert!(!alpha_eq(&mul(vec![r.clone(), r.clone(), p.clone()]), &w, &mul(vec![r, p]), &w));
    }
}
