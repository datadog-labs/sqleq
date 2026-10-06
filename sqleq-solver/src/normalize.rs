// Unless explicitly stated otherwise all files in this repository are licensed under the
// Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/).
// Copyright 2026-Present Datadog, Inc.

//! Normalization: rewrite one side's closed term toward a canonical sum-of-products form so rung
//! 2's alpha-equivalence ([`crate::alpha`]) can compare the two sides. A port of the sound core of
//! SQLSolver's `UNormalization` + `QueryUExprNormalizer`, not a transcription: rules that are
//! unsound as written there (a POWER inversion, an equality accepted against the wrong term, a
//! squash remainder dropped when splitting summations) are left out, and every rule is a pure
//! function over `Rc`-shared terms instead of an in-place mutation.
//!
//! Two scoping decisions carry the soundness argument:
//! * **Binders are renamed apart** before every round ([`Normalizer::rename_apart`]). `Rc` sharing
//!   and the translator's reuse of a relation's term (e.g. `JOIN`'s null-padding branch) leave the
//!   same `Base` id bound in several places; the summation rules (promote, merge) are only correct
//!   when every binder is unique, which is Java's `renameSameBoundedVarSummation` precondition too.
//! * **Only multiplicity positions are rewritten.** The children of `Add`/`Mul`/`Squash`/`Neg`/`Sum`
//!   reached from the root are multiplicities (non-negative integers); `Pred`/`Func` arguments are
//!   values, where `Add`/`Mul` also encode arithmetic and CASE selection over non-numeric values.
//!   Rules that are identities of the multiplicity semiring are therefore never applied to values.

use std::collections::{HashMap, HashSet};
use std::hash::{Hash, Hasher};
use std::rc::Rc;

use crate::ic::Ics;
use crate::translate::{NUMERIC_EQ_KEY, OUT_VAR_ID};
use crate::uterm::{mk_mul, mk_sum, PredKind, UConst, UTerm, UVar};

/// Rounds of (simplify, eliminate bound vars, rename apart) before giving up on a fixpoint. Stopping
/// early is sound -- every intermediate term is equivalent to the input -- it only costs proofs.
const MAX_ROUNDS: usize = 64;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NormalizeError {
    /// The term grew past the tree-size budget (sum-of-products distribution is exponential).
    TooLarge,
}

pub struct Normalizer {
    /// Column count of every base var the current term mentions.
    pub widths: HashMap<u32, usize>,
    max_tree: usize,
    /// Integrity constraints to rewrite with; empty for none. An explicit parameter, where Java
    /// selects them through static state (`QueryUExprICRewriter.selectIC`).
    ics: Ics,
}

impl Normalizer {
    pub fn new(widths: HashMap<u32, usize>, max_tree: usize) -> Self {
        Normalizer { widths, max_tree, ics: Ics::default() }
    }

    /// Rewrites with these constraints as well. The result is then only equivalent to the input
    /// on databases that satisfy them -- which are the only databases a query runs on.
    pub fn with_ics(mut self, ics: Ics) -> Self {
        self.ics = ics;
        self
    }

    pub fn normalize(&mut self, t: &UTerm) -> Result<UTerm, NormalizeError> {
        let t = if self.ics.keys.is_empty() { t.clone() } else { mark_set_tables(t, &self.ics) };
        let mut cur = self.rename_apart(&t);
        for _ in 0..MAX_ROUNDS {
            let next = self.round(&cur);
            if next.tree_size(self.max_tree + 1) > self.max_tree {
                return Err(NormalizeError::TooLarge);
            }
            if next == cur {
                return Ok(next);
            }
            cur = next;
        }
        Ok(cur)
    }

    /// One round: local rules, bound-var elimination, renaming apart. Public so tooling can trace
    /// how a term evolves; [`Normalizer::normalize`] is the entry point.
    pub fn round(&mut self, t: &UTerm) -> UTerm {
        let mut next = simplify(t);
        if !self.ics.not_null.is_empty() {
            next = self.remove_not_null(&next);
        }
        if !self.ics.keys.is_empty() {
            next = self.drop_key_squash(&next);
        }
        let next = canonicalize_congruence(&next);
        let next = self.contradict_neg_sum(&next);
        let next = self.eliminate_bound(&next);
        let next = self.merge_complements(&next, false);
        let next = self.rename_apart(&next);
        // Squash spines strip set markers (`‖T(x)‖` inside a squash is `T(x)`), and removing a
        // squash can expose such an atom again; re-marking every round keeps one canonical form.
        if self.ics.keys.is_empty() {
            next
        } else {
            mark_set_tables(&next, &self.ics)
        }
    }

    /// A unique key makes a squash redundant (`applyUniqueAddSquash*`, the direction that removes
    /// one): `‖Σ_V body‖` is `Σ_V body` itself when the sum can only be 0 or 1. That holds when every
    /// factor of the body is 0/1 and every bound var `v` of `V` has a keyed table factor `T(v)` whose
    /// key columns the body's equalities tie to terms mentioning no var of `V` -- then at most one
    /// row of each `T` can match, so at most one assignment of `V` survives.
    fn drop_key_squash(&self, t: &UTerm) -> UTerm {
        match t {
            UTerm::Squash(c) => {
                let inner = self.drop_key_squash(c);
                if let UTerm::Sum { vars, body } = &inner {
                    if self.sum_is_zero_one(vars, body) {
                        return inner;
                    }
                }
                UTerm::Squash(Rc::new(inner))
            }
            UTerm::Add(ts) => UTerm::Add(ts.iter().map(|c| Rc::new(self.drop_key_squash(c))).collect()),
            UTerm::Mul(ts) => UTerm::Mul(ts.iter().map(|c| Rc::new(self.drop_key_squash(c))).collect()),
            UTerm::Neg(c) => UTerm::Neg(Rc::new(self.drop_key_squash(c))),
            UTerm::Sum { vars, body } => UTerm::Sum { vars: vars.clone(), body: Rc::new(self.drop_key_squash(body)) },
            other => pass_args(other, &mut |x| self.drop_key_squash(x)),
        }
    }

    fn sum_is_zero_one(&self, vars: &[UVar], body: &UTerm) -> bool {
        let factors = factors_of(body);
        // A set table's atom is 0/1 even when a squash spine has stripped its `‖·‖` marker.
        let zero_one = |f: &UTerm| is_zero_one(f) || matches!(f, UTerm::Table { name, .. } if self.ics.is_set(name));
        if !factors.iter().all(zero_one) {
            return false;
        }
        let bound = base_ids(vars);
        let tables = direct_tables(body);
        let classes = Congruence::from_factors(&factors);
        bound.iter().all(|&v| {
            tables.iter().filter(|(_, y)| *y == v).any(|(name, _)| {
                self.ics.keys.get(name).into_iter().flatten().any(|key| {
                    key.iter().all(|&c| {
                        classes.members_of(&UTerm::Var(UVar::proj(c, UVar::Base(v)))).iter().any(|m| !mentions_any(m, &bound))
                    })
                })
            })
        })
    }

    /// `applyNotNullRemoveNotNull`: in a product holding a table factor `T(x)` (directly, or inside a
    /// squash factor), every NOT NULL column of `x` -- and every term the product's equalities tie to
    /// it -- is non-null, so its `[· = Null]` tests are 0 throughout the product. The product is 0
    /// whenever `T(x)` is, and when it is not, `x` is a row of `T`.
    fn remove_not_null(&self, t: &UTerm) -> UTerm {
        match t {
            UTerm::Mul(fs) => {
                let mut factors: Vec<UTerm> = fs.iter().map(|f| (**f).clone()).collect();
                let mut guaranteed = Vec::new();
                for f in &factors {
                    guaranteed_tables(f, &mut guaranteed);
                }
                let mut nonnull: HashSet<UTerm> = HashSet::new();
                for (name, var) in &guaranteed {
                    for c in self.ics.not_null.get(name).into_iter().flatten() {
                        nonnull.insert(UTerm::Var(UVar::proj(*c, var.clone())));
                    }
                }
                if !nonnull.is_empty() {
                    let classes = Congruence::from_factors(&factors);
                    let seeds: Vec<UTerm> = nonnull.iter().cloned().collect();
                    for s in &seeds {
                        nonnull.extend(classes.members_of(s).into_iter().cloned());
                    }
                    factors = factors.iter().map(|f| replace_null_tests(f, &nonnull)).collect();
                }
                UTerm::Mul(factors.iter().map(|f| Rc::new(self.remove_not_null(f))).collect())
            }
            UTerm::Add(ts) => UTerm::Add(ts.iter().map(|c| Rc::new(self.remove_not_null(c))).collect()),
            UTerm::Squash(c) => UTerm::Squash(Rc::new(self.remove_not_null(c))),
            UTerm::Neg(c) => UTerm::Neg(Rc::new(self.remove_not_null(c))),
            UTerm::Sum { vars, body } => UTerm::Sum { vars: vars.clone(), body: Rc::new(self.remove_not_null(body)) },
            other => pass_args(other, &mut |x| self.remove_not_null(x)),
        }
    }

    /// Gives every `Sum` binder a fresh id, numbered in pre-order from 0 so that an unchanged term
    /// renames to itself (the fixpoint test relies on it). Free vars -- the output var -- keep their
    /// id. The result is a tree: sharing is given up here, which the caller's size budget bounds.
    pub fn rename_apart(&mut self, t: &UTerm) -> UTerm {
        let mut out_widths = HashMap::new();
        if let Some(w) = self.widths.get(&OUT_VAR_ID) {
            out_widths.insert(OUT_VAR_ID, *w);
        }
        let mut next = 0u32;
        let renamed = rename(t, &HashMap::new(), &mut next, &self.widths, &mut out_widths);
        self.widths = out_widths;
        renamed
    }

    /// Removes bound vars whose every column is fixed by the equalities of their own sum's body
    /// (`QueryUExprNormalizer.removeDeterminedBoundedVarByTuple`/`ByConst`), and single columns so
    /// fixed (`removeDeterminedBoundedColumnByConst`). Inner sums first.
    fn eliminate_bound(&mut self, t: &UTerm) -> UTerm {
        match t {
            UTerm::Const(_) | UTerm::Var(_) | UTerm::Table { .. } => t.clone(),
            UTerm::Pred { .. } | UTerm::Func { .. } => pass_args(t, &mut |x| self.eliminate_bound(x)),
            UTerm::Add(ts) => UTerm::Add(ts.iter().map(|c| Rc::new(self.eliminate_bound(c))).collect()),
            UTerm::Mul(ts) => UTerm::Mul(ts.iter().map(|c| Rc::new(self.eliminate_bound(c))).collect()),
            UTerm::Squash(c) => UTerm::Squash(Rc::new(self.eliminate_bound(c))),
            UTerm::Neg(c) => UTerm::Neg(Rc::new(self.eliminate_bound(c))),
            UTerm::Sum { vars, body } => {
                let body = self.eliminate_bound(body);
                self.eliminate_at(vars.clone(), body)
            }
        }
    }

    fn eliminate_at(&mut self, mut vars: Vec<UVar>, mut body: UTerm) -> UTerm {
        while let Some((pos, narrower, next)) = self.eliminate_one(&vars, &body) {
            match narrower {
                Some(z) => vars[pos] = z,
                None => {
                    vars.remove(pos);
                }
            }
            body = next;
        }
        if vars.is_empty() {
            body
        } else {
            UTerm::Sum { vars, body: Rc::new(body) }
        }
    }

    /// The first bound var of `vars` that can be eliminated, or narrowed to a var of fewer columns
    /// (returned in its place), with the body it leaves behind.
    fn eliminate_one(&mut self, vars: &[UVar], body: &UTerm) -> Option<(usize, Option<UVar>, UTerm)> {
        let classes = Congruence::from_factors(&factors_of(body));
        for (pos, x) in vars.iter().enumerate() {
            let UVar::Base(xid) = *x else { continue };
            let Some(&width) = self.widths.get(&xid) else { continue };
            // By tuple: every column of x equals the same column of one other var y of the same
            // width, so x is y (a tuple is its columns). Table atoms of x become atoms of y.
            if let Some(y) = self.same_tuple(&classes, xid, width) {
                return Some((pos, None, body.rename_base(xid, y)));
            }
            // By key (`applyPrimaryImplyTupleEq`): x and y are both rows of a table with a unique
            // NOT NULL key K and agree on K, so they are the same row.
            if let Some(y) = self.same_key(&classes, body, xid) {
                return Some((pos, None, body.rename_base(xid, y)));
            }
            // By constant: every column of x equals some x-free term, and x is in no table atom (a
            // `Table` needs a var, not a tuple of terms). Exactly one x satisfies the equalities, so
            // the sum collapses to the body at that tuple.
            if !mentions_table_of(body, xid) {
                let cols: Option<Vec<UTerm>> = (0..width as u32)
                    .map(|i| classes.pick_free_of(&UTerm::Var(UVar::proj(i, UVar::Base(xid))), xid))
                    .collect();
                if let Some(b) = cols.and_then(|cols| subst_cols(body, xid, &cols)) {
                    // Folded now, while the copies of each substituted term are still identical:
                    // the defining `[x.i = e]` has become `[e = e]`.
                    return Some((pos, None, simplify(&b)));
                }
                // By column: one column x.i equals a term e that does not mention x.i (other
                // columns of x may occur in it). For each choice of the other columns exactly one
                // x.i leaves the body non-zero, so the sum ranges over the other columns only, as a
                // fresh var z one column narrower -- fresh, because a copy of this sum elsewhere
                // keeps x. e is renumbered onto z first; its placeholder for x.i is never read.
                if width > 1 {
                    let col = |i: usize, v: u32| UTerm::Var(UVar::proj(i as u32, UVar::Base(v)));
                    for i in 0..width {
                        let Some(e) = classes.pick_free_of_col(&col(i, xid), xid, i as u32) else { continue };
                        let z = self.fresh_id();
                        let mut cols: Vec<UTerm> =
                            (0..width).map(|j| if j == i { UTerm::Const(UConst::Null) } else { col(j - (j > i) as usize, z) }).collect();
                        let Some(e) = subst_cols(&e, xid, &cols) else { continue };
                        cols[i] = e;
                        if let Some(b) = subst_cols(body, xid, &cols) {
                            self.widths.insert(z, width - 1);
                            return Some((pos, Some(UVar::Base(z)), simplify(&b)));
                        }
                    }
                }
            }
        }
        None
    }

    /// An id no var of the current term has.
    fn fresh_id(&self) -> u32 {
        self.widths.keys().filter(|&&id| id != OUT_VAR_ID).max().map_or(0, |m| m + 1)
    }

    /// Another var y with a direct `T(y)` factor for the same keyed table as a direct `T(x)`, agreeing
    /// with x on every column of one of T's keys.
    fn same_key(&self, classes: &Congruence, body: &UTerm, xid: u32) -> Option<u32> {
        if self.ics.keys.is_empty() {
            return None;
        }
        let tables = direct_tables(body);
        let (name, _) = tables.iter().find(|(_, v)| *v == xid)?;
        let keys = self.ics.keys.get(name)?;
        let col = |i: u32, v: u32| UTerm::Var(UVar::proj(i, UVar::Base(v)));
        tables
            .iter()
            .filter(|(n, y)| n == name && *y != xid)
            .map(|(_, y)| *y)
            .find(|&y| keys.iter().any(|k| k.iter().all(|&c| classes.same(&col(c, xid), &col(c, y)))))
    }

    fn same_tuple(&self, classes: &Congruence, xid: u32, width: usize) -> Option<u32> {
        let col = |i: usize, v: u32| UTerm::Var(UVar::proj(i as u32, UVar::Base(v)));
        let mut candidates: Vec<u32> = classes
            .members_of(&col(0, xid))
            .into_iter()
            .filter_map(|t| match t {
                UTerm::Var(UVar::Proj { index: 0, base }) => match **base {
                    UVar::Base(y) if y != xid => Some(y),
                    _ => None,
                },
                _ => None,
            })
            .collect();
        candidates.sort_unstable();
        candidates.dedup();
        candidates
            .into_iter()
            .find(|&y| self.widths.get(&y) == Some(&width) && (0..width).all(|i| classes.same(&col(i, xid), &col(i, y))))
    }

    /// `simplifySumToZeroByContradictNegSum`: a product with a factor `¬Σ_Y b` is 0 when its other
    /// factors already hold a witness for the sum -- an assignment σ of `Y` to rows in scope under
    /// which every factor of `b` is one of the other factors, or an equality theirs imply. Wherever
    /// the other factors are non-zero, `b[σ] ≥ 1`, and `b[σ]` is one summand of `Σ_Y b`, so the
    /// negation is 0. `b` must be a count, so that no summand is negative. This is how a `LEFT JOIN`
    /// whose row an inner join has already matched loses its null-padded branch.
    fn contradict_neg_sum(&self, t: &UTerm) -> UTerm {
        match t {
            UTerm::Mul(fs) => {
                let factors: Vec<UTerm> = fs.iter().map(|f| self.contradict_neg_sum(f)).collect();
                if (0..factors.len()).any(|i| self.witnessed(&factors, i)) {
                    return int(0);
                }
                UTerm::Mul(factors.into_iter().map(Rc::new).collect())
            }
            UTerm::Add(ts) => UTerm::Add(ts.iter().map(|c| Rc::new(self.contradict_neg_sum(c))).collect()),
            UTerm::Squash(c) => UTerm::Squash(Rc::new(self.contradict_neg_sum(c))),
            UTerm::Neg(c) => UTerm::Neg(Rc::new(self.contradict_neg_sum(c))),
            UTerm::Sum { vars, body } => UTerm::Sum { vars: vars.clone(), body: Rc::new(self.contradict_neg_sum(body)) },
            other => pass_args(other, &mut |x| self.contradict_neg_sum(x)),
        }
    }

    /// Whether `factors[i]` is `¬Σ_Y b` and the other factors hold a witness for `Σ_Y b`. Each
    /// var of `Y` may be sent to any row the other factors scan from the same table, of the same
    /// width; σ need not be injective, since any assignment of `Y` is one point of the sum.
    fn witnessed(&self, factors: &[UTerm], i: usize) -> bool {
        let UTerm::Neg(n) = &factors[i] else { return false };
        let UTerm::Sum { vars, body } = &**n else { return false };
        if !is_count(body) {
            return false;
        }
        let others: Vec<UTerm> = factors.iter().enumerate().filter(|(j, _)| *j != i).map(|(_, f)| f.clone()).collect();
        let scanned = direct_tables(&UTerm::Mul(others.iter().cloned().map(Rc::new).collect()));
        let inner = direct_tables(body);
        let mut choices: Vec<(u32, Vec<u32>)> = Vec::new();
        for v in vars {
            let UVar::Base(y) = *v else { return false };
            let Some(width) = self.widths.get(&y) else { return false };
            let mut rows: Vec<u32> = inner
                .iter()
                .filter(|(_, id)| *id == y)
                .flat_map(|(name, _)| scanned.iter().filter(move |(n, _)| n == name).map(|(_, z)| *z))
                .filter(|z| self.widths.get(z) == Some(width))
                .collect();
            rows.sort_unstable();
            rows.dedup();
            if rows.is_empty() {
                return false;
            }
            choices.push((y, rows));
        }
        let assignments = choices.iter().try_fold(1usize, |n, (_, rows)| n.checked_mul(rows.len()));
        if assignments.is_none_or(|n| n > MAX_WITNESSES) {
            return false;
        }
        let held: HashSet<&UTerm> = others.iter().collect();
        let classes = Congruence::from_factors(&others);
        let mut pick = vec![0usize; choices.len()];
        loop {
            let b = choices.iter().zip(&pick).fold((**body).clone(), |b, ((y, rows), &k)| b.rename_base(*y, rows[k]));
            if factors_of(&b).iter().all(|g| implied_nonzero(g, &held, &classes)) {
                return true;
            }
            // Next assignment, odometer-style; done once every position has wrapped.
            let Some(pos) = (0..pick.len()).find(|&p| pick[p] + 1 < choices[p].1.len()) else { return false };
            pick[pos] += 1;
            pick[..pos].iter_mut().for_each(|k| *k = 0);
        }
    }
}

impl Normalizer {
    /// Complementary summands: `Σ_V X·N + Σ_V X·¬N` is `Σ_V X` when `N` is 0/1, since then
    /// `N + ¬N = 1`. This is how a `LEFT JOIN` that can match at most one row, and whose columns
    /// nothing reads, drops out: its matched and null-padded branches add up to the row alone. The
    /// matched branch is rebuilt from the padded one with this round's own rules and recognised by
    /// alpha-equivalence, rung 2's own test, so a merge is only as trusted as a rung-2 proof.
    ///
    /// `set_ctx`: `t` sits on a squash or negation spine of non-negative terms, where only zero
    /// versus non-zero matters. There `N` need not be 0/1: with `X, N ≥ 0`, `X·N + X·¬N` is non-zero
    /// exactly when `X` is, since `N + ¬N ≥ 1`.
    fn merge_complements(&self, t: &UTerm, set_ctx: bool) -> UTerm {
        match t {
            UTerm::Add(ts) => {
                let mut parts: Vec<UTerm> = ts.iter().map(|c| self.merge_complements(c, set_ctx)).collect();
                if parts.len() <= MAX_MERGE_SUMMANDS {
                    while let Some((p, q, merged)) = self.complement_pair(&parts, set_ctx) {
                        parts.remove(p.max(q));
                        parts.remove(p.min(q));
                        parts.push(merged);
                    }
                }
                match parts.len() {
                    1 => parts.pop().expect("len 1"),
                    _ => UTerm::Add(parts.into_iter().map(Rc::new).collect()),
                }
            }
            UTerm::Mul(ts) => UTerm::Mul(ts.iter().map(|c| Rc::new(self.merge_complements(c, set_ctx))).collect()),
            UTerm::Squash(c) => UTerm::Squash(Rc::new(self.merge_complements(c, spine_nonneg(c)))),
            UTerm::Neg(c) => UTerm::Neg(Rc::new(self.merge_complements(c, spine_nonneg(c)))),
            UTerm::Sum { vars, body } => {
                UTerm::Sum { vars: vars.clone(), body: Rc::new(self.merge_complements(body, set_ctx)) }
            }
            other => pass_args(other, &mut |x| self.merge_complements(x, false)),
        }
    }

    /// The first `(P, Q, Σ_V X)` with `Q = Σ_V X·¬N` for a 0/1 sum `N` (any sum, in a set context)
    /// and `P` alpha-equivalent to `Σ_V X·N`. `V` may be empty, `Q` then being a bare product.
    fn complement_pair(&self, parts: &[UTerm], set_ctx: bool) -> Option<(usize, usize, UTerm)> {
        // Compared without set-table markers: this round's squash spines have stripped them in some
        // places and not others, and on a set table `‖T(x)‖` is `T(x)`.
        let unmarked: Vec<UTerm> = parts.iter().map(|p| unmark_set_tables(p, &self.ics)).collect();
        let mut shapes: Vec<Option<u64>> = vec![None; parts.len()];
        // Which tables each summand scans directly. The rules that rebuild the matched summand
        // merge and rename table atoms but never add or drop a table, so a summand scanning other
        // tables cannot be the match -- a filter that skips the rebuild, which is the cost here.
        let scans: Vec<Vec<String>> = parts.iter().map(|p| table_names(&factors_of(summand_body(p)))).collect();
        for (qi, q) in parts.iter().enumerate() {
            let (vars, body) = match q {
                UTerm::Sum { vars, body } => (vars.clone(), (**body).clone()),
                other => (Vec::new(), other.clone()),
            };
            let factors = factors_of(&body);
            for (ni, f) in factors.iter().enumerate() {
                // Only a negated sum -- the "no row matched" of an outer join. A negated predicate
                // (`X·p + X·¬p`) would be as exact, but a product has many of those (every NULL
                // test) and rebuilding a summand for each costs more than it finds.
                let UTerm::Neg(n) = f else { continue };
                let UTerm::Sum { vars: u, body: y } = &**n else { continue };
                let zero_one = self.sum_is_zero_one(u, y);
                if !(zero_one || (set_ctx && spine_nonneg(n))) {
                    continue;
                }
                let rest: Vec<UTerm> = factors.iter().enumerate().filter(|(j, _)| *j != ni).map(|(_, g)| g.clone()).collect();
                let mut would_scan = rest.clone();
                would_scan.extend(factors_of(summand_body(n)));
                let would_scan = table_names(&would_scan);
                if !scans.iter().enumerate().any(|(pi, s)| pi != qi && *s == would_scan) {
                    continue;
                }
                let (matched, matched_widths) = self.local_rules(&mk_sum(vars.clone(), mk_mul(rest.iter().cloned().chain([(**n).clone()]))));
                let matched = unmark_set_tables(&matched, &self.ics);
                let want = crate::alpha::shape(&matched);
                let hit = unmarked.iter().enumerate().find(|(pi, p)| {
                    *pi != qi
                        && scans[*pi] == would_scan
                        && *shapes[*pi].get_or_insert_with(|| crate::alpha::shape(p)) == want
                        && crate::alpha::alpha_eq(p, &self.widths, &matched, &matched_widths)
                });
                if let Some((pi, _)) = hit {
                    return Some((pi, qi, mk_sum(vars, mk_mul(rest))));
                }
            }
        }
        None
    }

    /// A round's rules up to bound-var elimination, repeated to a fixpoint (pulling a sum out of a
    /// product leaves its body nested one level until the next pass), so that a term rebuilt from a
    /// normalized one reaches the form its neighbours already have. Every rule preserves meaning.
    /// Runs on a copy of the widths, since elimination may mint vars, and returns them with it.
    fn local_rules(&self, t: &UTerm) -> (UTerm, HashMap<u32, usize>) {
        let mut scratch = Normalizer { widths: self.widths.clone(), max_tree: self.max_tree, ics: self.ics.clone() };
        let mut cur = t.clone();
        for _ in 0..MAX_LOCAL_PASSES {
            let mut next = simplify(&cur);
            if !scratch.ics.not_null.is_empty() {
                next = scratch.remove_not_null(&next);
            }
            if !scratch.ics.keys.is_empty() {
                next = scratch.drop_key_squash(&next);
            }
            let next = scratch.contradict_neg_sum(&canonicalize_congruence(&next));
            let next = scratch.eliminate_bound(&next);
            if next == cur {
                break;
            }
            cur = next;
        }
        (cur, scratch.widths)
    }
}

/// Passes [`Normalizer::local_rules`] makes over a rebuilt term.
const MAX_LOCAL_PASSES: usize = 8;

/// A summand's product: the body of a sum, or the term itself.
fn summand_body(t: &UTerm) -> &UTerm {
    match t {
        UTerm::Sum { body, .. } => body,
        other => other,
    }
}

/// The distinct names of the tables `factors` scan directly (`T(x)` or `‖T(x)‖`), sorted.
fn table_names(factors: &[UTerm]) -> Vec<String> {
    let mut names: Vec<String> = direct_tables(&UTerm::Mul(factors.iter().cloned().map(Rc::new).collect())).into_iter().map(|(n, _)| n).collect();
    names.sort_unstable();
    names.dedup();
    names
}

/// Summands of one `Add` beyond which [`Normalizer::merge_complements`] does not look for pairs.
const MAX_MERGE_SUMMANDS: usize = 64;

/// Assignments [`Normalizer::witnessed`] tries for one negated sum before giving up.
const MAX_WITNESSES: usize = 64;

/// Whether a factor `g` of a count is at least 1 wherever the factors `held` are all non-zero: it is
/// one of them (a count that is non-zero is at least 1), a squash of one or the operand of a squashed
/// one (`‖h‖ ≠ 0` iff `h ≠ 0`), an equality `held`'s equalities imply, or a positive constant.
fn implied_nonzero(g: &UTerm, held: &HashSet<&UTerm>, classes: &Congruence) -> bool {
    if held.contains(g) {
        return true;
    }
    match g {
        UTerm::Const(UConst::Int(n)) => *n >= 1,
        UTerm::Pred { kind: PredKind::Eq, args } if args.len() == 2 => classes.same(&args[0], &args[1]),
        UTerm::Squash(h) => held.contains(&**h),
        other => held.contains(&UTerm::Squash(Rc::new(other.clone()))),
    }
}

// -- renaming ------------------------------------------------------------------------------------

fn rename(
    t: &UTerm,
    map: &HashMap<u32, u32>,
    next: &mut u32,
    widths: &HashMap<u32, usize>,
    out_widths: &mut HashMap<u32, usize>,
) -> UTerm {
    let var = |v: &UVar| rename_var(v, map);
    match t {
        UTerm::Const(_) => t.clone(),
        UTerm::Var(v) => UTerm::Var(var(v)),
        UTerm::Table { name, var: v } => UTerm::Table { name: name.clone(), var: var(v) },
        UTerm::Pred { kind, args } => {
            UTerm::Pred { kind: *kind, args: args.iter().map(|a| rename(a, map, next, widths, out_widths)).collect() }
        }
        UTerm::Func { name, args } => {
            UTerm::Func { name: name.clone(), args: args.iter().map(|a| rename(a, map, next, widths, out_widths)).collect() }
        }
        UTerm::Add(ts) => UTerm::Add(ts.iter().map(|c| Rc::new(rename(c, map, next, widths, out_widths))).collect()),
        UTerm::Mul(ts) => UTerm::Mul(ts.iter().map(|c| Rc::new(rename(c, map, next, widths, out_widths))).collect()),
        UTerm::Squash(c) => UTerm::Squash(Rc::new(rename(c, map, next, widths, out_widths))),
        UTerm::Neg(c) => UTerm::Neg(Rc::new(rename(c, map, next, widths, out_widths))),
        UTerm::Sum { vars, body } => {
            let mut inner = map.clone();
            let mut fresh = Vec::with_capacity(vars.len());
            for v in vars {
                if let UVar::Base(old) = v {
                    let new = *next;
                    *next += 1;
                    if let Some(w) = widths.get(old) {
                        out_widths.insert(new, *w);
                    }
                    inner.insert(*old, new);
                    fresh.push(UVar::Base(new));
                }
            }
            UTerm::Sum { vars: fresh, body: Rc::new(rename(body, &inner, next, widths, out_widths)) }
        }
    }
}

fn rename_var(v: &UVar, map: &HashMap<u32, u32>) -> UVar {
    match v {
        UVar::Base(id) => UVar::Base(*map.get(id).unwrap_or(id)),
        UVar::Proj { index, base } => UVar::Proj { index: *index, base: Box::new(rename_var(base, map)) },
    }
}

// -- local simplification ------------------------------------------------------------------------

/// The sort key for children and `Eq` arguments: a structural hash that ignores bound var ids (only
/// the free output var's id counts), like Java's `hashForSort`. It must not see bound ids:
/// [`Normalizer::rename_apart`] numbers binders in traversal order, so an id-sensitive key lets the
/// order and the numbering feed each other and the term oscillates instead of reaching a fixpoint.
/// With stable sorts, children that tie keep their previous order.
fn key(t: &UTerm) -> u64 {
    fn var(v: &UVar, h: &mut std::collections::hash_map::DefaultHasher) {
        match v {
            UVar::Base(id) => (*id == OUT_VAR_ID).hash(h),
            UVar::Proj { index, base } => {
                index.hash(h);
                var(base, h);
            }
        }
    }
    fn go(t: &UTerm, h: &mut std::collections::hash_map::DefaultHasher) {
        std::mem::discriminant(t).hash(h);
        match t {
            UTerm::Const(c) => c.hash(h),
            UTerm::Var(v) => var(v, h),
            UTerm::Table { name, var: v } => {
                name.hash(h);
                var(v, h);
            }
            UTerm::Pred { kind, args } => {
                kind.hash(h);
                args.iter().for_each(|a| go(a, h));
            }
            UTerm::Func { name, args } => {
                name.hash(h);
                args.iter().for_each(|a| go(a, h));
            }
            UTerm::Add(ts) | UTerm::Mul(ts) => ts.iter().for_each(|c| go(c, h)),
            UTerm::Squash(c) | UTerm::Neg(c) => go(c, h),
            UTerm::Sum { vars, body } => {
                vars.len().hash(h);
                go(body, h);
            }
        }
    }
    let mut h = std::collections::hash_map::DefaultHasher::new();
    go(t, &mut h);
    h.finish()
}

/// Drops repeated 0/1 factors (`x·x = x` for those), keeping the first of each.
fn dedup_zero_one(factors: &mut Vec<UTerm>) {
    let mut seen: HashSet<UTerm> = HashSet::new();
    factors.retain(|f| !is_zero_one(f) || seen.insert(f.clone()));
}

/// 0/1-valued by construction: predicates, squashes, negations, the constants 0 and 1, and products
/// of those. Not `Table` (a bag multiplicity), not `Add`, not `Sum`.
pub fn is_zero_one(t: &UTerm) -> bool {
    match t {
        UTerm::Const(UConst::Int(0 | 1)) | UTerm::Pred { .. } | UTerm::Squash(_) | UTerm::Neg(_) => true,
        UTerm::Mul(fs) => fs.iter().all(|f| is_zero_one(f)),
        _ => false,
    }
}

fn int(n: i64) -> UTerm {
    UTerm::Const(UConst::Int(n))
}

/// The number a constant `[key(c)]` under SQL's numeric `=` key compares by.
fn numeric_key_operand(t: &UTerm) -> Option<crate::uterm::Number> {
    match t {
        UTerm::Func { name, args } if name == NUMERIC_EQ_KEY => match args.as_slice() {
            [UTerm::Const(c)] => c.number(),
            _ => None,
        },
        _ => None,
    }
}

/// Folds `Pred`s decidable from their arguments alone. Mirrors [`crate::eval`]'s semantics: `Eq`
/// is identity (`Null` equals `Null`), so it folds only when the two constants are the same value
/// or certainly different ones -- `1` against `1.0` is neither, being equal numbers of different
/// types (see [`UConst::same_value`]). An order comparison holds only between two numbers, and is
/// decided exactly. One between two strings is left alone: its answer depends on the collation,
/// which the IR does not carry, and only `C` orders by bytes. SQL's `=` between two numbers is
/// identity of their numeric-key images, which do fold, by exact value.
fn simplify_pred(kind: PredKind, args: &[UTerm]) -> UTerm {
    let (kind, a, b) = match (kind, args) {
        (PredKind::Gt, [a, b]) => (PredKind::Lt, b, a),
        (PredKind::Ge, [a, b]) => (PredKind::Le, b, a),
        (k, [a, b]) => (k, a, b),
        _ => return UTerm::Pred { kind, args: args.to_vec() },
    };
    if let (UTerm::Const(x), UTerm::Const(y)) = (a, b) {
        let folded = match kind {
            PredKind::Eq => x.same_value(y),
            PredKind::Ne => x.same_value(y).map(|same| !same),
            PredKind::Lt | PredKind::Le => match (x.number(), y.number()) {
                (Some(p), Some(q)) => Some(if kind == PredKind::Lt { p < q } else { p <= q }),
                _ if matches!((x, y), (UConst::Str(_), UConst::Str(_))) => None,
                // NULL, or a number against a string: never ordered.
                _ => Some(false),
            },
            PredKind::Gt | PredKind::Ge => unreachable!("oriented above"),
        };
        if let Some(holds) = folded {
            return int(holds as i64);
        }
    }
    if let (PredKind::Eq | PredKind::Ne, Some(p), Some(q)) = (kind, numeric_key_operand(a), numeric_key_operand(b)) {
        return int(((p == q) == (kind == PredKind::Eq)) as i64);
    }
    if a == b {
        match kind {
            PredKind::Eq => return int(1),
            PredKind::Ne | PredKind::Lt => return int(0),
            _ => {}
        }
    }
    // Identity is symmetric: orient `Eq`/`Ne` canonically, with a `Null` constant always second.
    let (a, b) = match kind {
        PredKind::Eq | PredKind::Ne => {
            let null_first = matches!(a, UTerm::Const(UConst::Null));
            let null_second = matches!(b, UTerm::Const(UConst::Null));
            if null_first || (!null_second && key(b) < key(a)) {
                (b, a)
            } else {
                (a, b)
            }
        }
        _ => (a, b),
    };
    UTerm::Pred { kind, args: vec![a.clone(), b.clone()] }
}

/// Every term reached by `+`, `×` and `Σ` from here is non-negative (no negative constant among
/// them), so `x > 0` does not depend on whether a nested `Squash` is kept.
fn spine_nonneg(t: &UTerm) -> bool {
    match t {
        UTerm::Const(UConst::Int(n)) => *n >= 0,
        UTerm::Const(_) | UTerm::Var(_) | UTerm::Func { .. } => false,
        UTerm::Table { .. } | UTerm::Pred { .. } | UTerm::Squash(_) | UTerm::Neg(_) => true,
        UTerm::Add(ts) | UTerm::Mul(ts) => ts.iter().all(|c| spine_nonneg(c)),
        UTerm::Sum { body, .. } => spine_nonneg(body),
    }
}

/// Under `Squash`/`Neg` only zero versus non-zero matters, so squashes nested along the
/// `+`/`×`/`Σ` spine can be dropped (`eliminateSquash`): with non-negative terms, `f(‖y‖) > 0` iff
/// `f(y) > 0` for any `f` built from those operators.
fn strip_squash(t: UTerm) -> UTerm {
    if !spine_nonneg(&t) {
        return t;
    }
    fn go(t: &UTerm) -> UTerm {
        match t {
            UTerm::Squash(c) => go(c),
            UTerm::Add(ts) => UTerm::Add(ts.iter().map(|c| Rc::new(go(c))).collect()),
            UTerm::Mul(ts) => UTerm::Mul(ts.iter().map(|c| Rc::new(go(c))).collect()),
            UTerm::Sum { vars, body } => UTerm::Sum { vars: vars.clone(), body: Rc::new(go(body)) },
            other => other.clone(),
        }
    }
    go(&t)
}

/// One bottom-up pass of local rules over multiplicity positions.
pub fn simplify(t: &UTerm) -> UTerm {
    match t {
        UTerm::Const(_) | UTerm::Var(_) | UTerm::Table { .. } => t.clone(),
        // The numeric `=` key depends on its operand's value alone, so a constant there may be any
        // constant of that value; one canonical spelling lets `a = 1.0` and `a = 1` meet.
        UTerm::Func { name, args } if name == NUMERIC_EQ_KEY => {
            let args = args
                .iter()
                .map(|a| match simplify_value(a) {
                    UTerm::Const(c) => UTerm::Const(c.numeric_canonical().unwrap_or(c)),
                    other => other,
                })
                .collect();
            UTerm::Func { name: name.clone(), args }
        }
        UTerm::Func { name, args } => UTerm::Func { name: name.clone(), args: args.iter().map(simplify_value).collect() },
        UTerm::Pred { kind, args } => simplify_pred(*kind, &args.iter().map(simplify_value).collect::<Vec<_>>()),
        UTerm::Squash(c) => match strip_squash(simplify(c)) {
            UTerm::Const(UConst::Int(n)) => int((n != 0) as i64),
            c if is_zero_one(&c) => c,
            c => UTerm::Squash(Rc::new(c)),
        },
        UTerm::Neg(c) => match strip_squash(simplify(c)) {
            UTerm::Const(UConst::Int(n)) => int((n == 0) as i64),
            // ¬¬x is "x ≠ 0", which is x itself only when x is already 0/1.
            UTerm::Neg(inner) if is_zero_one(&inner) => (*inner).clone(),
            UTerm::Neg(inner) => UTerm::Squash(inner),
            // De Morgan: a sum of non-negative terms is 0 iff every summand is, so ¬(a + b) is
            // ¬a · ¬b. Pushing negation inward turns one opaque factor into several, which is what
            // lets a separately stated condition (`c IS NOT NULL` beside `c = $2`) collapse into it.
            UTerm::Add(ts) if ts.iter().all(|t| spine_nonneg(t)) => {
                simplify_mul(ts.iter().map(|t| UTerm::Neg(t.clone())).collect())
            }
            c => UTerm::Neg(Rc::new(c)),
        },
        UTerm::Add(ts) => simplify_add(ts.iter().map(|c| simplify(c))),
        UTerm::Mul(ts) => simplify_mul(ts.iter().map(|c| simplify(c)).collect()),
        UTerm::Sum { vars, body } => simplify_sum(vars.clone(), simplify(body)),
    }
}

/// A multiplicity by construction: built only from table atoms, predicates, squashes, negations and
/// non-negative integers with `+`, `×` and `Σ`. Such a term denotes a natural number wherever it
/// sits, so even in a value position (a `COUNT`'s sum, say) every multiplicity rule applies to it.
/// Anything with a value leaf (`Var`, `Func`, a non-integer constant) is not.
fn is_count(t: &UTerm) -> bool {
    match t {
        UTerm::Table { .. } | UTerm::Pred { .. } | UTerm::Squash(_) | UTerm::Neg(_) => true,
        UTerm::Const(UConst::Int(n)) => *n >= 0,
        UTerm::Const(_) | UTerm::Var(_) | UTerm::Func { .. } => false,
        UTerm::Add(ts) | UTerm::Mul(ts) => ts.iter().all(|c| is_count(c)),
        UTerm::Sum { body, .. } => is_count(body),
    }
}

/// Applies a multiplicity-position pass to every maximal count-typed subterm below a value position,
/// rebuilding the value structure around them unchanged.
fn in_values(t: &UTerm, pass: &mut dyn FnMut(&UTerm) -> UTerm) -> UTerm {
    if is_count(t) {
        return pass(t);
    }
    match t {
        UTerm::Add(ts) => UTerm::Add(ts.iter().map(|c| Rc::new(in_values(c, pass))).collect()),
        UTerm::Mul(ts) => UTerm::Mul(ts.iter().map(|c| Rc::new(in_values(c, pass))).collect()),
        UTerm::Sum { vars, body } => UTerm::Sum { vars: vars.clone(), body: Rc::new(in_values(body, pass)) },
        UTerm::Func { name, args } => UTerm::Func { name: name.clone(), args: args.iter().map(|a| in_values(a, pass)).collect() },
        other => other.clone(),
    }
}

/// A `Pred`/`Func` node with a multiplicity-position pass applied inside its arguments.
fn pass_args(t: &UTerm, pass: &mut dyn FnMut(&UTerm) -> UTerm) -> UTerm {
    match t {
        UTerm::Pred { kind, args } => UTerm::Pred { kind: *kind, args: args.iter().map(|a| in_values(a, pass)).collect() },
        UTerm::Func { name, args } => UTerm::Func { name: name.clone(), args: args.iter().map(|a| in_values(a, pass)).collect() },
        other => other.clone(),
    }
}

/// One bottom-up pass over a value position (a `Pred`/`Func` argument). There `Add`/`Mul`/`Sum` may
/// be arithmetic or CASE selection over non-numeric values, so they get only rules that hold under
/// both readings -- flattening, `0` annihilating a product, dropping `1` factors and `0` summands,
/// deduplicating 0/1 factors, ordering -- and never distribution, summation promotion, or constant
/// arithmetic. Count-typed subterms ([`is_count`]) are multiplicities wherever they sit, so they get
/// the full rules.
fn simplify_value(t: &UTerm) -> UTerm {
    if is_count(t) {
        return simplify(t);
    }
    match t {
        UTerm::Add(ts) => {
            let mut flat: Vec<UTerm> = Vec::new();
            for c in ts.iter().map(|c| simplify_value(c)) {
                match c {
                    UTerm::Add(inner) => flat.extend(inner.iter().map(|x| (**x).clone())),
                    UTerm::Const(UConst::Int(0)) => {}
                    other => flat.push(other),
                }
            }
            flat.sort_by_key(key);
            match flat.len() {
                0 => int(0),
                1 => flat.pop().expect("len 1"),
                _ => UTerm::Add(flat.into_iter().map(Rc::new).collect()),
            }
        }
        UTerm::Mul(ts) => {
            let mut flat: Vec<UTerm> = Vec::new();
            for c in ts.iter().map(|c| simplify_value(c)) {
                match c {
                    UTerm::Mul(inner) => flat.extend(inner.iter().map(|x| (**x).clone())),
                    UTerm::Const(UConst::Int(0)) => return int(0),
                    UTerm::Const(UConst::Int(1)) => {}
                    other => flat.push(other),
                }
            }
            flat.sort_by_key(key);
            dedup_zero_one(&mut flat);
            match flat.len() {
                0 => int(1),
                1 => flat.pop().expect("len 1"),
                _ => UTerm::Mul(flat.into_iter().map(Rc::new).collect()),
            }
        }
        UTerm::Sum { vars, body } => match simplify_value(body) {
            UTerm::Const(UConst::Int(0)) => int(0),
            b => UTerm::Sum { vars: vars.clone(), body: Rc::new(b) },
        },
        other => simplify(other),
    }
}

fn simplify_add(parts: impl Iterator<Item = UTerm>) -> UTerm {
    let mut flat: Vec<UTerm> = Vec::new();
    let mut constant: i64 = 0;
    for p in parts {
        match p {
            UTerm::Add(inner) => flat.extend(inner.iter().map(|c| (**c).clone())),
            UTerm::Const(UConst::Int(n)) if constant.checked_add(n).is_some() => constant += n,
            other => flat.push(other),
        }
    }
    if constant != 0 {
        flat.push(int(constant));
    }
    flat.sort_by_key(key);
    match flat.len() {
        0 => int(0),
        1 => flat.pop().expect("len 1"),
        _ => UTerm::Add(flat.into_iter().map(Rc::new).collect()),
    }
}

fn simplify_mul(parts: Vec<UTerm>) -> UTerm {
    let mut flat: Vec<UTerm> = Vec::new();
    let mut constant: i64 = 1;
    for p in parts {
        match p {
            UTerm::Mul(inner) => flat.extend(inner.iter().map(|c| (**c).clone())),
            UTerm::Const(UConst::Int(0)) => return int(0),
            UTerm::Const(UConst::Int(n)) if constant.checked_mul(n).is_some() => constant *= n,
            other => flat.push(other),
        }
    }
    flat.sort_by_key(key);
    // A 0/1 factor is idempotent (x·x = x); anything else keeps its multiplicity.
    dedup_zero_one(&mut flat);
    // Distribute over the first sum among the factors: Π·(a + b) → Π·a + Π·b.
    if let Some(pos) = flat.iter().position(|f| matches!(f, UTerm::Add(_))) {
        let UTerm::Add(summands) = flat.remove(pos) else { unreachable!() };
        if constant != 1 {
            flat.push(int(constant));
        }
        return UTerm::Add(
            summands
                .iter()
                .map(|s| {
                    let mut product = flat.clone();
                    product.push((**s).clone());
                    Rc::new(UTerm::Mul(product.into_iter().map(Rc::new).collect()))
                })
                .collect(),
        );
    }
    // Pull a sum outward: Π·Σ_v f → Σ_v (Π·f), when no other factor mentions v.
    if let Some(pos) = flat.iter().position(|f| matches!(f, UTerm::Sum { .. })) {
        let UTerm::Sum { vars, body } = flat[pos].clone() else { unreachable!() };
        let bound: HashSet<u32> = base_ids(&vars);
        let captured = flat.iter().enumerate().any(|(i, f)| i != pos && mentions_any(f, &bound));
        if !captured {
            flat.remove(pos);
            flat.push((*body).clone());
            if constant != 1 {
                flat.push(int(constant));
            }
            return UTerm::Sum { vars, body: Rc::new(UTerm::Mul(flat.into_iter().map(Rc::new).collect())) };
        }
    }
    if constant != 1 {
        flat.push(int(constant));
    }
    match flat.len() {
        0 => int(1),
        1 => flat.pop().expect("len 1"),
        _ => UTerm::Mul(flat.into_iter().map(Rc::new).collect()),
    }
}

fn simplify_sum(vars: Vec<UVar>, body: UTerm) -> UTerm {
    if vars.is_empty() {
        return body;
    }
    match body {
        UTerm::Const(UConst::Int(0)) => int(0),
        // Σ_v (a + b) = Σ_v a + Σ_v b, keeping every binder on both sides.
        UTerm::Add(ts) => UTerm::Add(ts.iter().map(|t| Rc::new(UTerm::Sum { vars: vars.clone(), body: t.clone() })).collect()),
        // Σ_x Σ_y f = Σ_{x,y} f, when the binders are distinct.
        UTerm::Sum { vars: inner, body } if base_ids(&vars).is_disjoint(&base_ids(&inner)) => {
            let mut all = vars;
            all.extend(inner);
            UTerm::Sum { vars: all, body }
        }
        other => UTerm::Sum { vars, body: Rc::new(other) },
    }
}

// -- variable helpers ----------------------------------------------------------------------------

fn base_ids(vars: &[UVar]) -> HashSet<u32> {
    vars.iter().filter_map(|v| if let UVar::Base(id) = v { Some(*id) } else { None }).collect()
}

fn var_base(v: &UVar) -> u32 {
    match v {
        UVar::Base(id) => *id,
        UVar::Proj { base, .. } => var_base(base),
    }
}

/// Whether `t` mentions any of `ids` (bound or free).
pub fn mentions_any(t: &UTerm, ids: &HashSet<u32>) -> bool {
    match t {
        UTerm::Const(_) => false,
        UTerm::Var(v) | UTerm::Table { var: v, .. } => ids.contains(&var_base(v)),
        UTerm::Pred { args, .. } | UTerm::Func { args, .. } => args.iter().any(|a| mentions_any(a, ids)),
        UTerm::Add(ts) | UTerm::Mul(ts) => ts.iter().any(|c| mentions_any(c, ids)),
        UTerm::Squash(c) | UTerm::Neg(c) => mentions_any(c, ids),
        UTerm::Sum { vars, body } => base_ids(vars).iter().any(|v| ids.contains(v)) || mentions_any(body, ids),
    }
}

fn mentions(t: &UTerm, id: u32) -> bool {
    mentions_any(t, &HashSet::from([id]))
}

/// Whether `t` mentions column `i` of `id`, counting any use of `id` as a whole tuple (a table
/// atom, a binder) as one.
fn mentions_col(t: &UTerm, id: u32, i: u32) -> bool {
    match t {
        UTerm::Const(_) => false,
        UTerm::Var(UVar::Proj { index, base }) if **base == UVar::Base(id) => *index == i,
        UTerm::Var(v) | UTerm::Table { var: v, .. } => var_base(v) == id,
        UTerm::Pred { args, .. } | UTerm::Func { args, .. } => args.iter().any(|a| mentions_col(a, id, i)),
        UTerm::Add(ts) | UTerm::Mul(ts) => ts.iter().any(|c| mentions_col(c, id, i)),
        UTerm::Squash(c) | UTerm::Neg(c) => mentions_col(c, id, i),
        UTerm::Sum { vars, body } => base_ids(vars).contains(&id) || mentions_col(body, id, i),
    }
}

fn mentions_table_of(t: &UTerm, id: u32) -> bool {
    match t {
        UTerm::Table { var, .. } => var_base(var) == id,
        UTerm::Const(_) | UTerm::Var(_) => false,
        UTerm::Pred { args, .. } | UTerm::Func { args, .. } => args.iter().any(|a| mentions_table_of(a, id)),
        UTerm::Add(ts) | UTerm::Mul(ts) => ts.iter().any(|c| mentions_table_of(c, id)),
        UTerm::Squash(c) | UTerm::Neg(c) | UTerm::Sum { body: c, .. } => mentions_table_of(c, id),
    }
}

/// Replaces every `Var(Proj(i, Base(id)))` by `cols[i]`. `None` if `id` also occurs any other way
/// (a whole-tuple use, or a binder), which the substitution could not express.
fn subst_cols(t: &UTerm, id: u32, cols: &[UTerm]) -> Option<UTerm> {
    Some(match t {
        UTerm::Const(_) => t.clone(),
        UTerm::Var(UVar::Proj { index, base }) if **base == UVar::Base(id) => cols.get(*index as usize)?.clone(),
        UTerm::Var(v) | UTerm::Table { var: v, .. } if var_base(v) == id => return None,
        UTerm::Var(_) | UTerm::Table { .. } => t.clone(),
        UTerm::Pred { kind, args } => {
            UTerm::Pred { kind: *kind, args: args.iter().map(|a| subst_cols(a, id, cols)).collect::<Option<_>>()? }
        }
        UTerm::Func { name, args } => {
            UTerm::Func { name: name.clone(), args: args.iter().map(|a| subst_cols(a, id, cols)).collect::<Option<_>>()? }
        }
        UTerm::Add(ts) => UTerm::Add(ts.iter().map(|c| subst_cols(c, id, cols).map(Rc::new)).collect::<Option<_>>()?),
        UTerm::Mul(ts) => UTerm::Mul(ts.iter().map(|c| subst_cols(c, id, cols).map(Rc::new)).collect::<Option<_>>()?),
        UTerm::Squash(c) => UTerm::Squash(Rc::new(subst_cols(c, id, cols)?)),
        UTerm::Neg(c) => UTerm::Neg(Rc::new(subst_cols(c, id, cols)?)),
        UTerm::Sum { vars, body } => {
            if base_ids(vars).contains(&id) {
                return None;
            }
            UTerm::Sum { vars: vars.clone(), body: Rc::new(subst_cols(body, id, cols)?) }
        }
    })
}

/// No var and no table anywhere inside: the same value in every environment (a parameter carrier
/// `QPn(..)` over literals, say).
fn is_closed(t: &UTerm) -> bool {
    match t {
        UTerm::Const(_) => true,
        UTerm::Var(_) | UTerm::Table { .. } | UTerm::Sum { .. } => false,
        UTerm::Pred { args, .. } | UTerm::Func { args, .. } => args.iter().all(is_closed),
        UTerm::Add(ts) | UTerm::Mul(ts) => ts.iter().all(|c| is_closed(c)),
        UTerm::Squash(c) | UTerm::Neg(c) => is_closed(c),
    }
}

/// A column, a constant, or a closed term: what congruence rewriting builds classes over. Only the
/// columns are ever substituted; constants and closed terms serve as representatives, which is how
/// `a = $1 AND a = b` comes to say `b = $1`.
fn is_atom(t: &UTerm) -> bool {
    matches!(t, UTerm::Var(UVar::Proj { .. })) || is_closed(t)
}

/// Representative preference: a constant, then a closed term, then a column of the (free) output
/// var, then any other column; then the id-agnostic key, so alpha-equivalent sides choose alike;
/// last a hash that sees ids, so the choice is deterministic. That last tie-break (two bound columns
/// alike but for their var) is stable across rounds because substitution never moves a binder, so
/// pre-order renaming gives the same vars the same ids again.
fn rep_rank(t: &UTerm) -> (u8, u64, u64) {
    let class = match t {
        UTerm::Const(_) => 0,
        UTerm::Var(v) if var_base(v) == OUT_VAR_ID => 2,
        UTerm::Var(_) => 3,
        _ => 1,
    };
    let mut h = std::collections::hash_map::DefaultHasher::new();
    t.hash(&mut h);
    (class, key(t), h.finish())
}

/// Congruence rewriting inside each product (the substitution half of Java's
/// `transformUnrelatedSummation`, and `propagateConstant` for direct equalities). The direct
/// `[a = b]` factors between atoms partition them into classes; within the product every member is
/// identical to every other wherever the product is non-zero, so each other occurrence of a column
/// member may be replaced by the class's representative. The class's own equalities are kept, in
/// the canonical star form `[member = rep]`, so no binding is lost. A class holding two distinct
/// constants makes the product 0 (`simplifyMultiplication`). One holding two constants that are
/// neither the same value nor certainly different ones (`1` and `1.0`, see
/// [`UConst::same_value`]) is left as it stands: no member is replaced, so neither constant is put
/// where the other was.
fn canonicalize_congruence(t: &UTerm) -> UTerm {
    match t {
        UTerm::Mul(fs) => {
            let factors: Vec<UTerm> = fs.iter().map(|f| (**f).clone()).collect();
            let rewritten = rewrite_product(factors);
            match rewritten {
                None => int(0),
                Some(fs) => UTerm::Mul(fs.iter().map(|f| Rc::new(canonicalize_congruence(f))).collect()),
            }
        }
        UTerm::Add(ts) => UTerm::Add(ts.iter().map(|c| Rc::new(canonicalize_congruence(c))).collect()),
        UTerm::Squash(c) => UTerm::Squash(Rc::new(canonicalize_congruence(c))),
        UTerm::Neg(c) => UTerm::Neg(Rc::new(canonicalize_congruence(c))),
        UTerm::Sum { vars, body } => UTerm::Sum { vars: vars.clone(), body: Rc::new(canonicalize_congruence(body)) },
        other => pass_args(other, &mut canonicalize_congruence),
    }
}

/// Factor propagation: in a product, a 0/1 factor `p` is 1 wherever the product is non-zero, so
/// every other occurrence of `p` inside the product may become 1; likewise a factor `¬q` (with `q`
/// 0/1) makes every other occurrence of `q` 0 (Java's `eliminateNegationTerm`). The factors
/// themselves stay, so nothing is lost, and scoping is safe because binders are unique.
fn propagate_factors(factors: Vec<UTerm>) -> Vec<UTerm> {
    let mut known: HashMap<UTerm, UTerm> = HashMap::new();
    for f in &factors {
        match f {
            UTerm::Neg(q) if is_zero_one(q) => {
                known.insert((**q).clone(), int(0));
            }
            p if matches!(p, UTerm::Pred { .. } | UTerm::Squash(_)) => {
                known.insert(p.clone(), int(1));
            }
            _ => {}
        }
    }
    if known.is_empty() {
        return factors;
    }
    let by_hash: HashMap<u64, Vec<(&UTerm, &UTerm)>> = known.iter().fold(HashMap::new(), |mut m, (k, v)| {
        m.entry(tree_hash(k)).or_default().push((k, v));
        m
    });
    factors
        .iter()
        .map(|f| {
            // Rewrite strictly inside each factor, never the factor itself (nor, for `¬q`, its `q`).
            let skip: &UTerm = match f {
                UTerm::Neg(q) => q,
                other => other,
            };
            let inside = |t: &UTerm| rebuild_children(t, &mut |c| replace_known(c, &by_hash, skip).0);
            match f {
                UTerm::Neg(q) => UTerm::Neg(Rc::new(inside(q))),
                other => inside(other),
            }
        })
        .collect()
}

/// A structural hash computed bottom-up by [`combine_hash`], so a traversal can hash every node for
/// the cost of hashing its children once.
fn tree_hash(t: &UTerm) -> u64 {
    let mut child_hashes = Vec::new();
    for_each_child(t, &mut |c| child_hashes.push(tree_hash(c)));
    combine_hash(t, &child_hashes)
}

fn combine_hash(t: &UTerm, child_hashes: &[u64]) -> u64 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    std::mem::discriminant(t).hash(&mut h);
    match t {
        UTerm::Const(c) => c.hash(&mut h),
        UTerm::Var(v) => v.hash(&mut h),
        UTerm::Table { name, var } => {
            name.hash(&mut h);
            var.hash(&mut h);
        }
        UTerm::Pred { kind, .. } => kind.hash(&mut h),
        UTerm::Func { name, .. } => name.hash(&mut h),
        UTerm::Sum { vars, .. } => vars.hash(&mut h),
        UTerm::Add(_) | UTerm::Mul(_) | UTerm::Squash(_) | UTerm::Neg(_) => {}
    }
    child_hashes.hash(&mut h);
    h.finish()
}

fn for_each_child(t: &UTerm, f: &mut dyn FnMut(&UTerm)) {
    match t {
        UTerm::Const(_) | UTerm::Var(_) | UTerm::Table { .. } => {}
        UTerm::Pred { args, .. } | UTerm::Func { args, .. } => args.iter().for_each(&mut *f),
        UTerm::Add(ts) | UTerm::Mul(ts) => ts.iter().for_each(|c| f(c)),
        UTerm::Squash(c) | UTerm::Neg(c) | UTerm::Sum { body: c, .. } => f(c),
    }
}

fn rebuild_children(t: &UTerm, f: &mut dyn FnMut(&UTerm) -> UTerm) -> UTerm {
    match t {
        UTerm::Const(_) | UTerm::Var(_) | UTerm::Table { .. } => t.clone(),
        UTerm::Pred { kind, args } => UTerm::Pred { kind: *kind, args: args.iter().map(&mut *f).collect() },
        UTerm::Func { name, args } => UTerm::Func { name: name.clone(), args: args.iter().map(&mut *f).collect() },
        UTerm::Add(ts) => UTerm::Add(ts.iter().map(|c| Rc::new(f(c))).collect()),
        UTerm::Mul(ts) => UTerm::Mul(ts.iter().map(|c| Rc::new(f(c))).collect()),
        UTerm::Squash(c) => UTerm::Squash(Rc::new(f(c))),
        UTerm::Neg(c) => UTerm::Neg(Rc::new(f(c))),
        UTerm::Sum { vars, body } => UTerm::Sum { vars: vars.clone(), body: Rc::new(f(body)) },
    }
}

/// Replaces known factors below `t` (never `skip` itself), returning the rewritten term and the
/// hash of the *original* `t`, computed bottom-up so no node is hashed twice.
fn replace_known(t: &UTerm, by_hash: &HashMap<u64, Vec<(&UTerm, &UTerm)>>, skip: &UTerm) -> (UTerm, u64) {
    let mut child_hashes = Vec::new();
    let rebuilt = rebuild_children(t, &mut |c| {
        let (r, h) = replace_known(c, by_hash, skip);
        child_hashes.push(h);
        r
    });
    let h = combine_hash(t, &child_hashes);
    if let Some(candidates) = by_hash.get(&h) {
        if let Some((_, v)) = candidates.iter().find(|(k, _)| *k == t && t != skip) {
            return ((*v).clone(), h);
        }
    }
    (rebuilt, h)
}

/// `None` when the product's equalities force two distinct constants together.
fn rewrite_product(factors: Vec<UTerm>) -> Option<Vec<UTerm>> {
    let factors = propagate_factors(factors);
    let mut defining: Vec<bool> = vec![false; factors.len()];
    let mut uf = Congruence { ids: HashMap::new(), parent: Vec::new() };
    for (i, f) in factors.iter().enumerate() {
        if let UTerm::Pred { kind: PredKind::Eq, args } = f {
            if let [a, b] = args.as_slice() {
                if is_atom(a) && is_atom(b) {
                    defining[i] = true;
                    let (x, y) = (uf.id(a), uf.id(b));
                    let (rx, ry) = (uf.find(x), uf.find(y));
                    uf.parent[rx] = ry;
                }
            }
        }
    }
    if !defining.contains(&true) {
        return Some(factors);
    }
    // Read the classes out in a fixed order: `HashMap` iteration order is randomized per instance,
    // and letting it choose representatives or the order of the star equalities makes the term
    // change every round without converging.
    let mut by_root: HashMap<usize, Vec<UTerm>> = HashMap::new();
    for (term, &i) in &uf.ids {
        by_root.entry(uf.find(i)).or_default().push(term.clone());
    }
    let mut classes: Vec<(usize, Vec<UTerm>)> = by_root.into_iter().collect();
    for (_, members) in &mut classes {
        members.sort_by_key(rep_rank);
    }
    classes.sort_by_key(|(_, members)| rep_rank(&members[0]));
    let mut subst: HashMap<UTerm, UTerm> = HashMap::new();
    let mut stars: Vec<UTerm> = Vec::new();
    let mut kept_roots: HashSet<usize> = HashSet::new();
    for (root, members) in &classes {
        let consts: Vec<&UConst> = members.iter().filter_map(|m| if let UTerm::Const(c) = m { Some(c) } else { None }).collect();
        // "Certainly different" is the complement of one equivalence (same kind and number, or
        // same string), so it shows between some two neighbours whenever it shows at all.
        if consts.windows(2).any(|w| w[0].same_value(w[1]) == Some(false)) {
            return None;
        }
        if consts.windows(2).any(|w| w[0].same_value(w[1]).is_none()) {
            kept_roots.insert(*root);
            continue;
        }
        let rep = members[0].clone();
        for m in members {
            if *m != rep {
                stars.push(simplify_pred(PredKind::Eq, &[m.clone(), rep.clone()]));
                if matches!(m, UTerm::Var(_)) {
                    subst.insert(m.clone(), rep.clone());
                }
            }
        }
    }
    // A defining equality of a class left alone stays as it was.
    let kept = |f: &UTerm| match f {
        UTerm::Pred { args, .. } => uf.ids.get(&args[0]).is_some_and(|&i| kept_roots.contains(&uf.find(i))),
        _ => false,
    };
    let mut out: Vec<UTerm> = factors
        .iter()
        .zip(&defining)
        .filter(|(f, d)| !**d || kept(f))
        .map(|(f, _)| substitute(f, &subst))
        .collect();
    out.extend(stars);
    Some(out)
}

/// Replaces every occurrence of a column that is a key of `subst` by its value, everywhere below
/// `t`. Only `Var` nodes are looked up: hashing every node would cost each subtree's full size
/// again at every level above it.
fn substitute(t: &UTerm, subst: &HashMap<UTerm, UTerm>) -> UTerm {
    if let UTerm::Var(_) = t {
        if let Some(r) = subst.get(t) {
            return r.clone();
        }
    }
    match t {
        UTerm::Const(_) | UTerm::Var(_) | UTerm::Table { .. } => t.clone(),
        UTerm::Pred { kind, args } => UTerm::Pred { kind: *kind, args: args.iter().map(|a| substitute(a, subst)).collect() },
        UTerm::Func { name, args } => UTerm::Func { name: name.clone(), args: args.iter().map(|a| substitute(a, subst)).collect() },
        UTerm::Add(ts) => UTerm::Add(ts.iter().map(|c| Rc::new(substitute(c, subst))).collect()),
        UTerm::Mul(ts) => UTerm::Mul(ts.iter().map(|c| Rc::new(substitute(c, subst))).collect()),
        UTerm::Squash(c) => UTerm::Squash(Rc::new(substitute(c, subst))),
        UTerm::Neg(c) => UTerm::Neg(Rc::new(substitute(c, subst))),
        UTerm::Sum { vars, body } => UTerm::Sum { vars: vars.clone(), body: Rc::new(substitute(body, subst)) },
    }
}

/// The inverse of [`mark_set_tables`]: every `‖T(x)‖` of a set table back to `T(x)`, the same
/// value on the databases the constraints describe.
fn unmark_set_tables(t: &UTerm, ics: &Ics) -> UTerm {
    if ics.keys.is_empty() {
        return t.clone();
    }
    match t {
        UTerm::Squash(c) if matches!(&**c, UTerm::Table { name, .. } if ics.is_set(name)) => (**c).clone(),
        UTerm::Const(_) | UTerm::Var(_) | UTerm::Table { .. } => t.clone(),
        other => rebuild_children(other, &mut |c| unmark_set_tables(c, ics)),
    }
}

/// Wraps every atom of a table that is a set (see [`Ics::is_set`]) as `‖T(x)‖`. That is the identity
/// on such tables, and it makes the atom structurally 0/1, so duplicate occurrences collapse like
/// any other idempotent factor.
fn mark_set_tables(t: &UTerm, ics: &Ics) -> UTerm {
    match t {
        UTerm::Table { name, .. } if ics.is_set(name) => UTerm::Squash(Rc::new(t.clone())),
        UTerm::Squash(c) if matches!(&**c, UTerm::Table { .. }) => t.clone(),
        UTerm::Const(_) | UTerm::Var(_) | UTerm::Table { .. } => t.clone(),
        UTerm::Pred { kind, args } => UTerm::Pred { kind: *kind, args: args.iter().map(|a| mark_set_tables(a, ics)).collect() },
        UTerm::Func { name, args } => UTerm::Func { name: name.clone(), args: args.iter().map(|a| mark_set_tables(a, ics)).collect() },
        UTerm::Add(ts) => UTerm::Add(ts.iter().map(|c| Rc::new(mark_set_tables(c, ics))).collect()),
        UTerm::Mul(ts) => UTerm::Mul(ts.iter().map(|c| Rc::new(mark_set_tables(c, ics))).collect()),
        UTerm::Squash(c) => UTerm::Squash(Rc::new(mark_set_tables(c, ics))),
        UTerm::Neg(c) => UTerm::Neg(Rc::new(mark_set_tables(c, ics))),
        UTerm::Sum { vars, body } => UTerm::Sum { vars: vars.clone(), body: Rc::new(mark_set_tables(body, ics)) },
    }
}

/// Tables a factor guarantees its var is a row of: a table atom, or one reached through squashes
/// and products (`‖c‖ > 0` means `c > 0`). Not through sums, negations or additions.
fn guaranteed_tables(f: &UTerm, out: &mut Vec<(String, UVar)>) {
    match f {
        UTerm::Table { name, var } => out.push((name.clone(), var.clone())),
        UTerm::Squash(c) => guaranteed_tables(c, out),
        UTerm::Mul(fs) => fs.iter().for_each(|c| guaranteed_tables(c, out)),
        _ => {}
    }
}

/// `(table, base id)` for every direct `T(x)` or `‖T(x)‖` factor of a product.
fn direct_tables(body: &UTerm) -> Vec<(String, u32)> {
    factors_of(body)
        .into_iter()
        .filter_map(|f| match f {
            UTerm::Table { name, var: UVar::Base(id) } => Some((name, id)),
            UTerm::Squash(c) => match &*c {
                UTerm::Table { name, var: UVar::Base(id) } => Some((name.clone(), *id)),
                _ => None,
            },
            _ => None,
        })
        .collect()
}

/// Replaces every `[t = Null]` whose `t` is known non-null by 0.
fn replace_null_tests(t: &UTerm, nonnull: &HashSet<UTerm>) -> UTerm {
    match t {
        UTerm::Pred { kind: PredKind::Eq, args } if args.len() == 2 => {
            let tested = match (&args[0], &args[1]) {
                (a, UTerm::Const(UConst::Null)) | (UTerm::Const(UConst::Null), a) => Some(a),
                _ => None,
            };
            if tested.is_some_and(|a| nonnull.contains(a)) {
                return int(0);
            }
            UTerm::Pred { kind: PredKind::Eq, args: args.iter().map(|a| replace_null_tests(a, nonnull)).collect() }
        }
        UTerm::Const(_) | UTerm::Var(_) | UTerm::Table { .. } => t.clone(),
        UTerm::Pred { kind, args } => UTerm::Pred { kind: *kind, args: args.iter().map(|a| replace_null_tests(a, nonnull)).collect() },
        UTerm::Func { name, args } => UTerm::Func { name: name.clone(), args: args.iter().map(|a| replace_null_tests(a, nonnull)).collect() },
        UTerm::Add(ts) => UTerm::Add(ts.iter().map(|c| Rc::new(replace_null_tests(c, nonnull))).collect()),
        UTerm::Mul(ts) => UTerm::Mul(ts.iter().map(|c| Rc::new(replace_null_tests(c, nonnull))).collect()),
        UTerm::Squash(c) => UTerm::Squash(Rc::new(replace_null_tests(c, nonnull))),
        UTerm::Neg(c) => UTerm::Neg(Rc::new(replace_null_tests(c, nonnull))),
        UTerm::Sum { vars, body } => UTerm::Sum { vars: vars.clone(), body: Rc::new(replace_null_tests(body, nonnull)) },
    }
}

/// The direct factors of a sum body: the conjuncts every non-zero summand of it satisfies.
fn factors_of(body: &UTerm) -> Vec<UTerm> {
    match body {
        UTerm::Mul(fs) => fs.iter().map(|f| (**f).clone()).collect(),
        other => vec![other.clone()],
    }
}

// -- congruence ----------------------------------------------------------------------------------

/// Equivalence classes of terms under the `[a = b]` factors of one product. Because each such
/// factor is a conjunct, every non-zero term of the product has `a` and `b` identical, so members
/// of a class may replace each other inside it.
struct Congruence {
    ids: HashMap<UTerm, usize>,
    parent: Vec<usize>,
}

impl Congruence {
    fn from_factors(factors: &[UTerm]) -> Self {
        let mut c = Congruence { ids: HashMap::new(), parent: Vec::new() };
        for f in factors {
            if let UTerm::Pred { kind: PredKind::Eq, args } = f {
                if let [a, b] = args.as_slice() {
                    let (x, y) = (c.id(a), c.id(b));
                    let (rx, ry) = (c.find(x), c.find(y));
                    c.parent[rx] = ry;
                }
            }
        }
        c
    }

    fn id(&mut self, t: &UTerm) -> usize {
        if let Some(&i) = self.ids.get(t) {
            return i;
        }
        let i = self.parent.len();
        self.parent.push(i);
        self.ids.insert(t.clone(), i);
        i
    }

    fn find(&self, mut i: usize) -> usize {
        while self.parent[i] != i {
            i = self.parent[i];
        }
        i
    }

    fn same(&self, a: &UTerm, b: &UTerm) -> bool {
        match (self.ids.get(a), self.ids.get(b)) {
            (Some(&x), Some(&y)) => self.find(x) == self.find(y),
            _ => a == b,
        }
    }

    fn members_of(&self, t: &UTerm) -> Vec<&UTerm> {
        let Some(&i) = self.ids.get(t) else { return Vec::new() };
        let root = self.find(i);
        self.ids.iter().filter(|(_, &j)| self.find(j) == root).map(|(m, _)| m).collect()
    }

    /// A member of `t`'s class that does not mention `id`, chosen deterministically (smallest, then
    /// by structural hash) so both runs of an unchanged term pick the same one.
    fn pick_free_of(&self, t: &UTerm, id: u32) -> Option<UTerm> {
        self.members_of(t)
            .into_iter()
            .filter(|m| !mentions(m, id))
            .min_by_key(|m| (m.tree_size(usize::MAX), key(m)))
            .cloned()
    }

    /// A member of `t`'s class that does not mention column `i` of `id`, chosen as
    /// [`Congruence::pick_free_of`] chooses. Other columns of `id` may occur in it.
    fn pick_free_of_col(&self, t: &UTerm, id: u32, i: u32) -> Option<UTerm> {
        self.members_of(t)
            .into_iter()
            .filter(|m| !mentions_col(m, id, i))
            .min_by_key(|m| (m.tree_size(usize::MAX), key(m)))
            .cloned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn col(i: u32, v: u32) -> UTerm {
        UTerm::Var(UVar::proj(i, UVar::Base(v)))
    }

    fn eq(a: UTerm, b: UTerm) -> UTerm {
        UTerm::Pred { kind: PredKind::Eq, args: vec![a, b] }
    }

    #[test]
    fn a_projection_of_every_column_collapses_to_the_table() {
        // Σ_x T(x)·[out.0 = x.0]·[out.1 = x.1]  →  T(out)
        let term = UTerm::Sum {
            vars: vec![UVar::Base(0)],
            body: Rc::new(UTerm::Mul(vec![
                Rc::new(UTerm::Table { name: "t".into(), var: UVar::Base(0) }),
                Rc::new(eq(col(0, OUT_VAR_ID), col(0, 0))),
                Rc::new(eq(col(1, OUT_VAR_ID), col(1, 0))),
            ])),
        };
        let widths = HashMap::from([(0, 2), (OUT_VAR_ID, 2)]);
        let got = Normalizer::new(widths, 1_000_000).normalize(&term).unwrap();
        assert_eq!(got, UTerm::Table { name: "t".into(), var: UVar::Base(OUT_VAR_ID) });
    }

    #[test]
    fn a_partial_projection_keeps_its_sum() {
        // Σ_x T(x)·[out.0 = x.0] with x two columns wide: x.1 is free, so x must stay bound.
        let term = UTerm::Sum {
            vars: vec![UVar::Base(0)],
            body: Rc::new(UTerm::Mul(vec![
                Rc::new(UTerm::Table { name: "t".into(), var: UVar::Base(0) }),
                Rc::new(eq(col(0, OUT_VAR_ID), col(0, 0))),
            ])),
        };
        let widths = HashMap::from([(0, 2), (OUT_VAR_ID, 1)]);
        let got = Normalizer::new(widths, 1_000_000).normalize(&term).unwrap();
        assert!(matches!(got, UTerm::Sum { .. }), "got {got:?}");
    }

    fn mul(fs: Vec<UTerm>) -> UTerm {
        UTerm::Mul(fs.into_iter().map(Rc::new).collect())
    }

    fn sum(vars: &[u32], body: UTerm) -> UTerm {
        UTerm::Sum { vars: vars.iter().map(|v| UVar::Base(*v)).collect(), body: Rc::new(body) }
    }

    fn table(v: u32) -> UTerm {
        UTerm::Table { name: "t".into(), var: UVar::Base(v) }
    }

    fn null() -> UTerm {
        UTerm::Const(UConst::Null)
    }

    #[test]
    fn a_not_null_column_of_a_scanned_row_is_never_null() {
        // Σ_x T(x)·¬[x.0 = Null]·[out.0 = x.0], with t.0 NOT NULL  →  T(out)
        let term = sum(&[0], mul(vec![table(0), UTerm::Neg(Rc::new(eq(col(0, 0), null()))), eq(col(0, OUT_VAR_ID), col(0, 0))]));
        let ics = Ics { not_null: HashMap::from([("t".into(), HashSet::from([0]))]), keys: HashMap::new() };
        let widths = HashMap::from([(0, 1), (OUT_VAR_ID, 1)]);
        let got = Normalizer::new(widths.clone(), 1_000_000).with_ics(ics).normalize(&term).unwrap();
        assert_eq!(got, table(OUT_VAR_ID));
        // Without the constraint the null test has to stay.
        let without = Normalizer::new(widths, 1_000_000).normalize(&term).unwrap();
        assert_ne!(without, table(OUT_VAR_ID));
    }

    #[test]
    fn a_self_join_on_a_key_is_the_table_itself() {
        // Σ_{x,y} T(x)·T(y)·[x.0 = y.0]·[out.0 = x.1]·[out.1 = y.1]   vs   Σ_x T(x)·[out.0 = x.1]·[out.1 = x.1]
        let joined = sum(
            &[0, 1],
            mul(vec![table(0), table(1), eq(col(0, 0), col(0, 1)), eq(col(0, OUT_VAR_ID), col(1, 0)), eq(col(1, OUT_VAR_ID), col(1, 1))]),
        );
        let single = sum(&[0], mul(vec![table(0), eq(col(0, OUT_VAR_ID), col(1, 0)), eq(col(1, OUT_VAR_ID), col(1, 0))]));
        let ics = Ics {
            not_null: HashMap::from([("t".into(), HashSet::from([0]))]),
            keys: HashMap::from([("t".into(), vec![vec![0]])]),
        };
        let norm = |t: &UTerm, widths: HashMap<u32, usize>, ics: Ics| {
            let mut n = Normalizer::new(widths, 1_000_000).with_ics(ics);
            let t = n.normalize(t).unwrap();
            (t, n.widths)
        };
        let (a, wa) = norm(&joined, HashMap::from([(0, 2), (1, 2), (OUT_VAR_ID, 2)]), ics.clone());
        let (b, wb) = norm(&single, HashMap::from([(0, 2), (OUT_VAR_ID, 2)]), ics);
        assert!(crate::alpha::alpha_eq(&a, &wa, &b, &wb), "{a:?}\n  vs\n{b:?}");
    }

    fn scan(name: &str, v: u32) -> UTerm {
        UTerm::Table { name: name.into(), var: UVar::Base(v) }
    }

    fn not(t: UTerm) -> UTerm {
        UTerm::Neg(Rc::new(t))
    }

    /// `t JOIN u ON t.1 = u.0 LEFT JOIN u AS u1 ON u1.0 = t.1`'s null-padded branch, with `extra`
    /// as further conditions on `u1`, and `joined` the table the inner join scans.
    fn padded_branch(joined: &str, extra: Vec<UTerm>) -> UTerm {
        let mut matched = vec![scan("u", 2), eq(col(0, 2), col(1, 0))];
        matched.extend(extra);
        sum(
            &[0, 1],
            mul(vec![scan("t", 0), scan(joined, 1), eq(col(1, 0), col(0, 1)), not(sum(&[2], mul(matched))), eq(col(0, OUT_VAR_ID), col(0, 0))]),
        )
    }

    #[test]
    fn a_left_join_the_inner_join_already_matched_has_no_padding() {
        let widths = HashMap::from([(0, 2), (1, 2), (2, 2), (OUT_VAR_ID, 1)]);
        let got = Normalizer::new(widths, 1_000_000).normalize(&padded_branch("u", vec![])).unwrap();
        assert_eq!(got, int(0));
    }

    #[test]
    fn a_witness_must_satisfy_every_condition_of_the_negated_sum() {
        // The joined row need not have u.1 = 5, so the left join may still pad.
        let widths = HashMap::from([(0, 2), (1, 2), (2, 2), (OUT_VAR_ID, 1)]);
        let extra = vec![eq(col(1, 2), int(5))];
        let got = Normalizer::new(widths, 1_000_000).normalize(&padded_branch("u", extra)).unwrap();
        assert_ne!(got, int(0));
    }

    #[test]
    fn a_witness_must_be_a_row_of_the_same_table_and_width() {
        let widths = HashMap::from([(0, 2), (1, 2), (2, 2), (OUT_VAR_ID, 1)]);
        let got = Normalizer::new(widths, 1_000_000).normalize(&padded_branch("v", vec![])).unwrap();
        assert_ne!(got, int(0));
        let widths = HashMap::from([(0, 2), (1, 3), (2, 2), (OUT_VAR_ID, 1)]);
        let got = Normalizer::new(widths, 1_000_000).normalize(&padded_branch("u", vec![])).unwrap();
        assert_ne!(got, int(0));
    }

    /// `t LEFT JOIN u ON u.0 = t.1`, reading `out.0` from `t.0` and, when `reads_u`, `out.1` from
    /// `u.1` (null on the padded branch): the matched branch plus the null-padded one.
    fn left_join(reads_u: bool) -> UTerm {
        let mut matched = vec![scan("t", 0), scan("u", 1), eq(col(0, 1), col(1, 0)), eq(col(0, OUT_VAR_ID), col(0, 0))];
        let mut padded = vec![scan("t", 2), not(sum(&[3], mul(vec![scan("u", 3), eq(col(0, 3), col(1, 2))]))), eq(col(0, OUT_VAR_ID), col(0, 2))];
        if reads_u {
            matched.push(eq(col(1, OUT_VAR_ID), col(1, 1)));
            padded.push(eq(col(1, OUT_VAR_ID), null()));
        }
        UTerm::Add(vec![Rc::new(sum(&[0, 1], mul(matched))), Rc::new(sum(&[2], mul(padded)))])
    }

    fn u_keyed_on_0() -> Ics {
        Ics {
            not_null: HashMap::from([("u".into(), HashSet::from([0]))]),
            keys: HashMap::from([("u".into(), vec![vec![0]])]),
        }
    }

    #[test]
    fn an_unread_left_join_on_a_key_is_the_left_table_alone() {
        let widths = HashMap::from([(0, 2), (1, 2), (2, 2), (3, 2), (OUT_VAR_ID, 1)]);
        let mut n = Normalizer::new(widths, 1_000_000).with_ics(u_keyed_on_0());
        let got = n.normalize(&left_join(false)).unwrap();
        assert_eq!(n.round(&got), got, "not a fixpoint");
        let alone = sum(&[0], mul(vec![scan("t", 0), eq(col(0, OUT_VAR_ID), col(0, 0))]));
        let mut m = Normalizer::new(HashMap::from([(0, 2), (OUT_VAR_ID, 1)]), 1_000_000).with_ics(u_keyed_on_0());
        let want = m.normalize(&alone).unwrap();
        assert!(crate::alpha::alpha_eq(&got, &n.widths, &want, &m.widths), "{got:?}\n  vs\n{want:?}");
    }

    #[test]
    fn a_left_join_without_a_key_or_whose_columns_are_read_keeps_both_branches() {
        let widths = HashMap::from([(0, 2), (1, 2), (2, 2), (3, 2), (OUT_VAR_ID, 2)]);
        // Without the key, a row of t may match two rows of u.
        let got = Normalizer::new(widths.clone(), 1_000_000).normalize(&left_join(false)).unwrap();
        assert!(matches!(got, UTerm::Add(_)), "got {got:?}");
        // With it, but reading u.1: the branches differ in what they output.
        let got = Normalizer::new(widths, 1_000_000).with_ics(u_keyed_on_0()).normalize(&left_join(true)).unwrap();
        assert!(matches!(got, UTerm::Add(_)), "got {got:?}");
    }

    #[test]
    fn under_a_squash_an_unread_left_join_needs_no_key() {
        // ‖matched + padded‖: only whether a row of t exists matters, however many rows of u match.
        let widths = HashMap::from([(0, 2), (1, 2), (2, 2), (3, 2), (OUT_VAR_ID, 1)]);
        let mut n = Normalizer::new(widths.clone(), 1_000_000);
        let got = n.normalize(&UTerm::Squash(Rc::new(left_join(false)))).unwrap();
        let alone = UTerm::Squash(Rc::new(sum(&[0], mul(vec![scan("t", 0), eq(col(0, OUT_VAR_ID), col(0, 0))]))));
        let mut m = Normalizer::new(HashMap::from([(0, 2), (OUT_VAR_ID, 1)]), 1_000_000);
        let want = m.normalize(&alone).unwrap();
        assert!(crate::alpha::alpha_eq(&got, &n.widths, &want, &m.widths), "{got:?}\n  vs\n{want:?}");
        // Reading u's columns still keeps the branches apart.
        let widths = HashMap::from([(0, 2), (1, 2), (2, 2), (3, 2), (OUT_VAR_ID, 2)]);
        let mut n = Normalizer::new(widths, 1_000_000);
        let got = n.normalize(&UTerm::Squash(Rc::new(left_join(true)))).unwrap();
        assert!(!crate::alpha::alpha_eq(&got, &n.widths, &want, &m.widths), "{got:?}");
    }

    /// `SELECT a FROM (SELECT a, b [, COUNT(*)] FROM t GROUP BY a, b) g`: the group var's key
    /// columns, plus a count column only when `counted`, and the group's existence.
    fn grouped(counted: bool) -> UTerm {
        let group = |v: u32| mul(vec![scan("t", v), eq(col(0, v), col(0, 0)), eq(col(1, v), col(1, 0))]);
        let mut fs = vec![eq(col(0, OUT_VAR_ID), col(0, 0)), UTerm::Squash(Rc::new(sum(&[1], group(1))))];
        if counted {
            fs.push(eq(col(2, 0), sum(&[2], group(2))));
        }
        sum(&[0], mul(fs))
    }

    #[test]
    fn an_unread_aggregate_column_of_a_group_drops_out() {
        let norm = |counted: bool| {
            let width = if counted { 3 } else { 2 };
            let mut n = Normalizer::new(HashMap::from([(0, width), (1, 2), (2, 2), (OUT_VAR_ID, 1)]), 1_000_000);
            let t = n.normalize(&grouped(counted)).unwrap();
            (t, n.widths)
        };
        let ((a, wa), (b, wb)) = (norm(true), norm(false));
        assert!(crate::alpha::alpha_eq(&a, &wa, &b, &wb), "{a:?}\n  vs\n{b:?}");
        // The unfixed key column b still ranges: one column narrower, but a sum.
        let UTerm::Sum { vars, .. } = &b else { panic!("got {b:?}") };
        let [UVar::Base(g)] = vars.as_slice() else { panic!("got {b:?}") };
        assert_eq!(wb.get(g), Some(&1));
    }

    #[test]
    fn double_negation_of_a_non_zero_one_term_becomes_a_squash() {
        let t = UTerm::Table { name: "t".into(), var: UVar::Base(OUT_VAR_ID) };
        let got = simplify(&UTerm::Neg(Rc::new(UTerm::Neg(Rc::new(t.clone())))));
        assert_eq!(got, UTerm::Squash(Rc::new(t)));
    }

    #[test]
    fn decimal_and_integer_constants_compare_numerically_only_under_sql_equality() {
        // `Eq` is identity, and `1.0` and `1` are equal numbers but not one value (`CAST(1.0 AS
        // TEXT)` is not `CAST(1 AS TEXT)`), so their identity is left open. SQL's `1.0 = 1`, the
        // numeric key's equality, is true; and two different numbers are different either way.
        let (one, one_dec) = (int(1), UTerm::Const(UConst::Decimal("1.0".into())));
        let open = UTerm::Pred { kind: PredKind::Eq, args: vec![one_dec.clone(), one.clone()] };
        assert_eq!(simplify_pred(PredKind::Eq, &[one_dec.clone(), one.clone()]), open);
        let key = |c: UTerm| simplify(&UTerm::Func { name: NUMERIC_EQ_KEY.into(), args: vec![c] });
        assert_eq!(simplify_pred(PredKind::Eq, &[key(one_dec.clone()), key(one.clone())]), int(1));
        assert_eq!(simplify_pred(PredKind::Eq, &[UTerm::Const(UConst::Decimal("1.5".into())), one]), int(0));
    }
}
