// Unless explicitly stated otherwise all files in this repository are licensed under the
// Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/).
// Copyright 2026-Present Datadog, Inc.

//! Rung 3: the set solver (SQLSolver's `SetSolver.java`).
//!
//! Where rung 2 compares two normalized terms syntactically, this rung asks Z3 whether they can
//! differ, for terms whose every summation sits under a squash or negation: there the U-expression
//! is plain first-order logic -- a sum is `∨`, a product `∧`, `Σ` is `∃` -- so the question is
//! decidable enough to hand to a solver. `¬(A = B)` UNSAT means the two agree everywhere.
//!
//! Unlike Java, applicability is not "translation happened not to throw": Java refuses every term
//! with a parameter carrier, a column inside a function, or an `IS NULL`, which rules out most real
//! queries. The encoding here:
//! * **one uninterpreted sort `Val`** for every value. Each canonical constant is its own `Val`
//!   constant, all pairwise distinct (distinct canonical constants are distinct values), `Null`
//!   among them. `Eq` is identity, `Ne` its negation, `Lt` an uninterpreted relation, and `Le` its
//!   complement with the operands swapped (`a <= b` is `¬(b < a)`). That is the one order fact
//!   encoded, and it holds wherever it is used: the translator puts every order comparison under
//!   its operands' not-null guard, and the non-NULL values of one type are totally ordered
//!   (preordered, under a collation that ties distinct strings).
//!   Functions and value-position arithmetic are uninterpreted `Val` functions -- sound, since the
//!   real ones are among their interpretations, and ours already take (null flag, value) pairs.
//! * **a table** `T` of width `w` is an uninterpreted `Val^w → Int`: its value at a tuple is the
//!   tuple's multiplicity, read as `> 0` in set positions.
//! * **a base var** of width `w` is `w` `Val` constants: bound by an `∃` where its sum is, free
//!   (hence universally quantified by the validity question) for the output var.
//!
//! Interpretations range over a superset of real databases (multiplicities may go negative in a
//! model), so UNSAT over them implies agreement on every real one; the extra models only cost
//! completeness.

use std::collections::HashMap;

use z3::ast::{self, Ast, Bool, Dynamic, Int};
use z3::{FuncDecl, Params, SatResult, Solver, Sort, Symbol};

use crate::translate::OUT_VAR_ID;
use crate::uterm::{PredKind, UConst, UTerm, UVar};

/// Z3's own per-query limit. Running out is "not proved", which is sound.
pub const TIMEOUT_MS: u32 = 3_000;

/// Whether rung 3 applies: every `Sum` in a multiplicity position sits under a `Squash`/`Neg`, and
/// no `Sum` sits in a value position (an aggregate or scalar-subquery value).
pub fn applicable(t: &UTerm) -> bool {
    fn count(t: &UTerm, under_set: bool) -> bool {
        match t {
            UTerm::Const(UConst::Int(_)) | UTerm::Table { .. } => true,
            UTerm::Const(_) | UTerm::Var(_) | UTerm::Func { .. } => false,
            UTerm::Pred { args, .. } => args.iter().all(value),
            UTerm::Add(ts) | UTerm::Mul(ts) => ts.iter().all(|c| count(c, under_set)),
            UTerm::Squash(c) | UTerm::Neg(c) => count(c, true),
            UTerm::Sum { body, .. } => under_set && count(body, true),
        }
    }
    fn value(t: &UTerm) -> bool {
        match t {
            UTerm::Const(_) | UTerm::Var(UVar::Proj { .. }) | UTerm::Table { .. } => true,
            UTerm::Var(UVar::Base(_)) | UTerm::Sum { .. } => false,
            UTerm::Pred { args, .. } | UTerm::Func { args, .. } => args.iter().all(value),
            UTerm::Add(ts) | UTerm::Mul(ts) => ts.iter().all(|c| value(c)),
            UTerm::Squash(c) | UTerm::Neg(c) => count(c, true),
        }
    }
    count(t, false)
}

/// `Some(true)` when Z3 shows the two terms equal on every database; `Some(false)` when it does not
/// (a counter-model, a timeout, or `unknown` -- never a disproof); `None` when the rung does not
/// apply to one of them.
///
/// Terms live in the calling thread's Z3 context, which the binding creates implicitly, so the time
/// limit and seed are set on the solver rather than on a context.
pub fn prove(a: &UTerm, widths_a: &HashMap<u32, usize>, b: &UTerm, widths_b: &HashMap<u32, usize>) -> Option<bool> {
    if !applicable(a) || !applicable(b) {
        return None;
    }
    let mut enc = Encoder::new();
    let ea = enc.count(a, Side::A, widths_a)?;
    let eb = enc.count(b, Side::B, widths_b)?;
    let solver = Solver::new();
    let mut params = Params::new();
    params.set_u32("timeout", TIMEOUT_MS);
    params.set_u32("random_seed", 9876);
    solver.set_params(&params);
    if let Some(distinct) = enc.distinct_constants() {
        solver.assert(&distinct);
    }
    solver.assert(ea.eq(&eb).not());
    Some(solver.check() == SatResult::Unsat)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum Side {
    A,
    B,
}

/// The canonical spelling of a constant's value, or `None` when there is no exact one: then the
/// constant is left out of the distinctness assertion, since declaring two equal values distinct
/// would let the solver prove false equalities. Integers and plain decimals share one spelling
/// (`1.0` is `1`), matching `eval::identical`.
fn canonical(c: &UConst) -> Option<String> {
    Some(match c {
        UConst::Int(n) => format!("n:{n}"),
        UConst::Decimal(s) => format!("n:{}", canonical_decimal(s)?),
        UConst::Str(s) => format!("s:{s}"),
        UConst::Null => "null".to_string(),
    })
}

fn canonical_decimal(s: &str) -> Option<String> {
    let (neg, digits) = match s.strip_prefix('-') {
        Some(rest) => (true, rest),
        None => (false, s.strip_prefix('+').unwrap_or(s)),
    };
    let (int, frac) = digits.split_once('.').unwrap_or((digits, ""));
    if int.is_empty() && frac.is_empty() || !int.bytes().chain(frac.bytes()).all(|b| b.is_ascii_digit()) {
        return None;
    }
    let int = int.trim_start_matches('0');
    let frac = frac.trim_end_matches('0');
    let int = if int.is_empty() { "0" } else { int };
    let body = if frac.is_empty() { int.to_string() } else { format!("{int}.{frac}") };
    Some(if neg && body != "0" { format!("-{body}") } else { body })
}

struct Encoder {
    val: Sort,
    /// Constants by canonical spelling (or, for one without, by a spelling of its own).
    consts: HashMap<String, (Dynamic, bool)>,
    /// The `Val` constants standing for each base var's columns. The output var is shared by both
    /// sides; every other var belongs to one side.
    vars: HashMap<(Option<Side>, u32), Vec<Dynamic>>,
    funcs: HashMap<String, FuncDecl>,
}

impl Encoder {
    fn new() -> Self {
        Encoder {
            val: Sort::uninterpreted(Symbol::String("Val".into())),
            consts: HashMap::new(),
            vars: HashMap::new(),
            funcs: HashMap::new(),
        }
    }

    fn distinct_constants(&self) -> Option<Bool> {
        let vals: Vec<&Dynamic> = self.consts.values().filter(|(_, exact)| *exact).map(|(v, _)| v).collect();
        (vals.len() >= 2).then(|| Dynamic::distinct(&vals))
    }

    /// Applies the function `name` (declared on first use with this signature) to `args`.
    fn apply(&mut self, name: String, domain: &[&Sort], range: &Sort, args: &[&dyn Ast]) -> Dynamic {
        self.funcs.entry(name.clone()).or_insert_with(|| FuncDecl::new(name, domain, range)).apply(args)
    }

    fn key(side: Side, id: u32) -> (Option<Side>, u32) {
        if id == OUT_VAR_ID {
            (None, id)
        } else {
            (Some(side), id)
        }
    }

    fn columns(&mut self, side: Side, id: u32, widths: &HashMap<u32, usize>) -> Option<Vec<Dynamic>> {
        let key = Self::key(side, id);
        if let Some(cols) = self.vars.get(&key) {
            return Some(cols.clone());
        }
        let width = *widths.get(&id)?;
        let cols: Vec<Dynamic> = (0..width).map(|_| Dynamic::fresh_const("c", &self.val)).collect();
        self.vars.insert(key, cols.clone());
        Some(cols)
    }

    fn constant(&mut self, c: &UConst) -> Dynamic {
        let (spelling, exact) = match canonical(c) {
            Some(s) => (s, true),
            None => (format!("raw:{c:?}"), false),
        };
        let val = &self.val;
        self.consts.entry(spelling).or_insert_with(|| (Dynamic::fresh_const("k", val), exact)).0.clone()
    }

    fn table(&mut self, name: &str, var: &UVar, side: Side, widths: &HashMap<u32, usize>) -> Option<Int> {
        let UVar::Base(id) = var else { return None };
        let cols = self.columns(side, *id, widths)?;
        let domain: Vec<Sort> = vec![self.val.clone(); cols.len()];
        let refs: Vec<&Sort> = domain.iter().collect();
        let args: Vec<&dyn Ast> = cols.iter().map(|c| c as &dyn Ast).collect();
        self.apply(format!("T:{name}/{}", cols.len()), &refs, &Sort::int(), &args).as_int()
    }

    /// A multiplicity position.
    fn count(&mut self, t: &UTerm, side: Side, widths: &HashMap<u32, usize>) -> Option<Int> {
        let one = Int::from_i64(1);
        let zero = Int::from_i64(0);
        Some(match t {
            UTerm::Const(UConst::Int(n)) => Int::from_i64(*n),
            UTerm::Table { name, var } => self.table(name, var, side, widths)?,
            UTerm::Pred { kind, args } => self.pred(*kind, args, side, widths)?.ite(&one, &zero),
            UTerm::Squash(c) => self.set(c, side, widths)?.ite(&one, &zero),
            UTerm::Neg(c) => self.set(c, side, widths)?.ite(&zero, &one),
            UTerm::Add(ts) => {
                let parts: Vec<Int> = ts.iter().map(|c| self.count(c, side, widths)).collect::<Option<_>>()?;
                Int::add(&parts)
            }
            UTerm::Mul(ts) => {
                let parts: Vec<Int> = ts.iter().map(|c| self.count(c, side, widths)).collect::<Option<_>>()?;
                Int::mul(&parts)
            }
            _ => return None,
        })
    }

    /// A set position: only whether the multiplicity is non-zero matters.
    fn set(&mut self, t: &UTerm, side: Side, widths: &HashMap<u32, usize>) -> Option<Bool> {
        Some(match t {
            UTerm::Const(UConst::Int(n)) => Bool::from_bool(*n != 0),
            UTerm::Table { name, var } => self.table(name, var, side, widths)?.gt(Int::from_i64(0)),
            UTerm::Pred { kind, args } => self.pred(*kind, args, side, widths)?,
            UTerm::Squash(c) => self.set(c, side, widths)?,
            UTerm::Neg(c) => self.set(c, side, widths)?.not(),
            UTerm::Add(ts) => {
                let parts: Vec<Bool> = ts.iter().map(|c| self.set(c, side, widths)).collect::<Option<_>>()?;
                Bool::or(&parts)
            }
            UTerm::Mul(ts) => {
                let parts: Vec<Bool> = ts.iter().map(|c| self.set(c, side, widths)).collect::<Option<_>>()?;
                Bool::and(&parts)
            }
            UTerm::Sum { vars, body } => {
                let mut bound: Vec<Dynamic> = Vec::new();
                for v in vars {
                    let UVar::Base(id) = v else { return None };
                    bound.extend(self.columns(side, *id, widths)?);
                }
                let body = self.set(body, side, widths)?;
                let refs: Vec<&dyn Ast> = bound.iter().map(|c| c as &dyn Ast).collect();
                ast::exists_const(&refs, &[], &body)
            }
            _ => return None,
        })
    }

    /// A value position (a `Pred`/`Func` argument).
    fn value(&mut self, t: &UTerm, side: Side, widths: &HashMap<u32, usize>) -> Option<Dynamic> {
        Some(match t {
            UTerm::Const(c) => self.constant(c),
            UTerm::Var(UVar::Proj { index, base }) => {
                let UVar::Base(id) = **base else { return None };
                self.columns(side, id, widths)?.get(*index as usize)?.clone()
            }
            UTerm::Func { name, args } => self.apply_val(format!("f:{name}"), args, side, widths)?,
            UTerm::Add(ts) => self.apply_val("v:+".to_string(), &ts.iter().map(|c| (**c).clone()).collect::<Vec<_>>(), side, widths)?,
            UTerm::Mul(ts) => self.apply_val("v:*".to_string(), &ts.iter().map(|c| (**c).clone()).collect::<Vec<_>>(), side, widths)?,
            // A multiplicity used as a value: some fixed injection of the integer into `Val`.
            UTerm::Pred { .. } | UTerm::Squash(_) | UTerm::Neg(_) | UTerm::Table { .. } => {
                let n = self.count(t, side, widths)?;
                let (int_sort, val) = (Sort::int(), self.val.clone());
                self.apply("int2val".to_string(), &[&int_sort], &val, &[&n])
            }
            UTerm::Var(UVar::Base(_)) | UTerm::Sum { .. } => return None,
        })
    }

    fn apply_val(&mut self, name: String, args: &[UTerm], side: Side, widths: &HashMap<u32, usize>) -> Option<Dynamic> {
        let vals: Vec<Dynamic> = args.iter().map(|a| self.value(a, side, widths)).collect::<Option<_>>()?;
        let domain: Vec<Sort> = vec![self.val.clone(); vals.len()];
        let refs: Vec<&Sort> = domain.iter().collect();
        let range = self.val.clone();
        let args: Vec<&dyn Ast> = vals.iter().map(|v| v as &dyn Ast).collect();
        Some(self.apply(format!("{name}/{}", vals.len()), &refs, &range, &args))
    }

    fn pred(&mut self, kind: PredKind, args: &[UTerm], side: Side, widths: &HashMap<u32, usize>) -> Option<Bool> {
        let [a, b] = args else { return None };
        let (va, vb) = (self.value(a, side, widths)?, self.value(b, side, widths)?);
        let rel = |enc: &mut Self, name: &str, x: &Dynamic, y: &Dynamic| -> Option<Bool> {
            let (val, boolean) = (enc.val.clone(), Sort::bool());
            enc.apply(name.to_string(), &[&val, &val], &boolean, &[x, y]).as_bool()
        };
        match kind {
            PredKind::Eq => Some(va.eq(&vb)),
            PredKind::Ne => Some(va.eq(&vb).not()),
            PredKind::Lt => rel(self, "lt", &va, &vb),
            PredKind::Le => rel(self, "lt", &vb, &va).map(|lt| lt.not()),
            PredKind::Gt => rel(self, "lt", &vb, &va),
            PredKind::Ge => rel(self, "lt", &va, &vb).map(|lt| lt.not()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::rc::Rc;

    fn col(i: u32, v: u32) -> UTerm {
        UTerm::Var(UVar::proj(i, UVar::Base(v)))
    }

    fn eq(a: UTerm, b: UTerm) -> UTerm {
        UTerm::Pred { kind: PredKind::Eq, args: vec![a, b] }
    }

    fn mul(fs: Vec<UTerm>) -> UTerm {
        UTerm::Mul(fs.into_iter().map(Rc::new).collect())
    }

    fn exists(v: u32, body: UTerm) -> UTerm {
        UTerm::Squash(Rc::new(UTerm::Sum { vars: vec![UVar::Base(v)], body: Rc::new(body) }))
    }

    fn table(name: &str, v: u32) -> UTerm {
        UTerm::Table { name: name.into(), var: UVar::Base(v) }
    }

    fn k(n: i64) -> UTerm {
        UTerm::Const(UConst::Int(n))
    }

    #[test]
    fn decimals_share_the_integer_spelling() {
        assert_eq!(canonical_decimal("001.500"), Some("1.5".into()));
        assert_eq!(canonical_decimal("2.0"), Some("2".into()));
        assert_eq!(canonical_decimal("-0.0"), Some("0".into()));
        assert_eq!(canonical_decimal("1e3"), None);
    }

    #[test]
    fn a_bag_sum_at_the_top_is_not_applicable() {
        let t = UTerm::Sum { vars: vec![UVar::Base(0)], body: Rc::new(table("r", 0)) };
        assert!(!applicable(&t));
        assert!(applicable(&exists(0, table("r", 0))));
    }

    #[test]
    fn a_condition_implied_inside_an_exists_is_proved_redundant() {
        // T(out)·‖∃y S(y)·[y.0 = out.0]·[out.0 = 1]‖  vs  T(out)·‖∃y S(y)·[y.0 = 1]·[out.0 = 1]‖
        let out = OUT_VAR_ID;
        let a = mul(vec![table("t", out), exists(0, mul(vec![table("s", 0), eq(col(0, 0), col(0, out)), eq(col(0, out), k(1))]))]);
        let b = mul(vec![table("t", out), exists(0, mul(vec![table("s", 0), eq(col(0, 0), k(1)), eq(col(0, out), k(1))]))]);
        let w = HashMap::from([(0, 1), (out, 1)]);
        assert_eq!(prove(&a, &w, &b, &w), Some(true));
    }

    #[test]
    fn different_constants_are_not_confused() {
        let out = OUT_VAR_ID;
        let a = mul(vec![table("t", out), eq(col(0, out), k(1))]);
        let b = mul(vec![table("t", out), eq(col(0, out), k(2))]);
        let w = HashMap::from([(out, 1)]);
        assert_eq!(prove(&a, &w, &b, &w), Some(false));
    }
}
