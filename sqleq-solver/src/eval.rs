// Unless explicitly stated otherwise all files in this repository are licensed under the
// Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/).
// Copyright 2026-Present Datadog, Inc.

//! A concrete evaluator for `UTerm`, used to test that translation and rewriting mean what they
//! claim: evaluate a term on a small database, before and after, and compare.
//!
//! Sums range over a finite universe of constants: `Σ_v body` enumerates every tuple of
//! `universe^width(v)`. That equals the true sum (over all tuples) whenever every tuple with a
//! non-zero summand lies inside the universe, which holds when the universe contains every constant
//! the term mentions and every value in every table. Tests are responsible for choosing it so.
//!
//! One evaluator serves both term roles. A multiplicity is an `Int`; a value is any constant. `Add`
//! and `Mul` over values follow the translator's selection idiom (`guard * value` summed over
//! mutually exclusive guards): zero factors annihilate, one factors and zero summands vanish, and
//! whatever single operand is left is the result; only several remaining integers do arithmetic.

use std::collections::HashMap;
use std::hash::{Hash, Hasher};

use crate::uterm::{PredKind, UConst, UTerm, UVar};

pub struct Db {
    pub tables: HashMap<String, Vec<Vec<UConst>>>,
    pub universe: Vec<UConst>,
    pub widths: HashMap<u32, usize>,
    /// Evaluation steps left before `eval` gives up with an error, so one oversized evaluation
    /// (nested sums enumerating the universe) cannot stall a caller.
    budget: std::cell::Cell<u64>,
}

/// Steps [`Db::new`] allows before giving up.
const DEFAULT_BUDGET: u64 = 5_000_000;

/// The current tuple of every bound (or free) base var.
pub type Env = HashMap<u32, Vec<UConst>>;

/// More assignments than this in one `Sum` means the test chose too large a universe.
const MAX_ASSIGNMENTS: usize = 1_000_000;

fn as_number(c: &UConst) -> Option<f64> {
    match c {
        UConst::Int(n) => Some(*n as f64),
        UConst::Decimal(s) => s.parse().ok(),
        _ => None,
    }
}

/// Identity: `Null` equals `Null`, numbers compare numerically, strings by value, and mixed kinds
/// never match. This is the `Pred::Eq` of the term algebra, not SQL `=`.
fn identical(a: &UConst, b: &UConst) -> bool {
    match (as_number(a), as_number(b)) {
        (Some(x), Some(y)) => x == y,
        _ => a == b,
    }
}

/// Both numbers or both strings: the pairs an order comparison is defined on (`Null` is in neither).
fn ordered(a: &UConst, b: &UConst) -> bool {
    matches!((a, b), (UConst::Str(_), UConst::Str(_))) || (as_number(a).is_some() && as_number(b).is_some())
}

fn less(a: &UConst, b: &UConst) -> bool {
    match (a, b) {
        (UConst::Str(x), UConst::Str(y)) => x < y,
        _ => matches!((as_number(a), as_number(b)), (Some(x), Some(y)) if x < y),
    }
}

/// The table a `Table{name, Base(id)}` factor of `body` (or `body` itself) restricts `id` to.
/// Also sees through a squash: `‖T(x)‖` is non-zero exactly where `T(x)` is.
fn table_guard(body: &UTerm, id: u32) -> Option<&str> {
    fn atom(t: &UTerm, id: u32) -> Option<&str> {
        match t {
            UTerm::Table { name, var: UVar::Base(v) } if *v == id => Some(name),
            UTerm::Squash(c) => atom(c, id),
            _ => None,
        }
    }
    match body {
        UTerm::Mul(fs) => fs.iter().find_map(|f| atom(f, id)),
        other => atom(other, id),
    }
}

impl Db {
    pub fn new(tables: HashMap<String, Vec<Vec<UConst>>>, universe: Vec<UConst>, widths: HashMap<u32, usize>) -> Db {
        Db { tables, universe, widths, budget: std::cell::Cell::new(DEFAULT_BUDGET) }
    }

    pub fn eval(&self, t: &UTerm, env: &mut Env) -> Result<UConst, String> {
        let left = self.budget.get().checked_sub(1).ok_or("evaluation budget exhausted")?;
        self.budget.set(left);
        Ok(match t {
            UTerm::Const(c) => c.clone(),
            UTerm::Var(v) => self.var(v, env)?,
            UTerm::Table { name, var } => {
                let row = self.tuple(var, env)?;
                let rows = self.tables.get(name).ok_or_else(|| format!("no table {name}"))?;
                UConst::Int(rows.iter().filter(|r| r.len() == row.len() && r.iter().zip(&row).all(|(a, b)| identical(a, b))).count() as i64)
            }
            UTerm::Pred { kind, args } => {
                let [a, b] = args.as_slice() else { return Err("pred arity".into()) };
                let (a, b) = (self.eval(a, env)?, self.eval(b, env)?);
                let holds = match kind {
                    PredKind::Eq => identical(&a, &b),
                    PredKind::Ne => !identical(&a, &b),
                    PredKind::Lt => less(&a, &b),
                    PredKind::Le => less(&a, &b) || (ordered(&a, &b) && identical(&a, &b)),
                    PredKind::Gt => less(&b, &a),
                    PredKind::Ge => less(&b, &a) || (ordered(&a, &b) && identical(&a, &b)),
                };
                UConst::Int(holds as i64)
            }
            UTerm::Func { name, args } => {
                // Some fixed interpretation of the uninterpreted symbol; any one will do, since a
                // sound rewrite must hold under all of them.
                let vals: Vec<UConst> = args.iter().map(|a| self.eval(a, env)).collect::<Result<_, _>>()?;
                let mut h = std::collections::hash_map::DefaultHasher::new();
                name.hash(&mut h);
                vals.hash(&mut h);
                self.universe[(h.finish() % self.universe.len() as u64) as usize].clone()
            }
            UTerm::Add(ts) => {
                let vals: Vec<UConst> = ts.iter().map(|c| self.eval(c, env)).collect::<Result<_, _>>()?;
                let rest: Vec<UConst> = vals.into_iter().filter(|v| *v != UConst::Int(0)).collect();
                match rest.as_slice() {
                    [] => UConst::Int(0),
                    [one] => one.clone(),
                    many => UConst::Int(many.iter().try_fold(0i64, |acc, v| match v {
                        UConst::Int(n) => acc.checked_add(*n).ok_or("overflow"),
                        _ => Err("non-integer summand"),
                    })?),
                }
            }
            UTerm::Mul(ts) => {
                // A zero factor annihilates the rest unevaluated, so a guarded value (a CASE branch,
                // a comparison under its operands' not-null guard) is read only where its guard
                // holds; a scalar subquery's value, a sum, has no reading where it is NULL.
                let mut vals: Vec<UConst> = Vec::with_capacity(ts.len());
                for c in ts {
                    match self.eval(c, env)? {
                        UConst::Int(0) => return Ok(UConst::Int(0)),
                        v => vals.push(v),
                    }
                }
                let rest: Vec<UConst> = vals.into_iter().filter(|v| *v != UConst::Int(1)).collect();
                match rest.as_slice() {
                    [] => UConst::Int(1),
                    [one] => one.clone(),
                    many => UConst::Int(many.iter().try_fold(1i64, |acc, v| match v {
                        UConst::Int(n) => acc.checked_mul(*n).ok_or("overflow"),
                        _ => Err("non-integer factor"),
                    })?),
                }
            }
            UTerm::Squash(c) => UConst::Int((self.count(c, env)? != 0) as i64),
            UTerm::Neg(c) => UConst::Int((self.count(c, env)? == 0) as i64),
            UTerm::Sum { vars, body } => UConst::Int(self.sum(vars, body, env)?),
        })
    }

    /// The one tuple var `id` can take, when a direct `[id.i = e]` factor of `body` gives every column
    /// with an `e` that already evaluates (it mentions no unassigned var). Any other tuple makes that
    /// factor 0, so this is exact.
    fn pinned(&self, body: &UTerm, id: u32, env: &Env) -> Option<Vec<UConst>> {
        self.pinned_cols(body, id, env)?.into_iter().collect()
    }

    /// Per column of `id`, the value a direct `[id.i = e]` factor of `body` fixes it to, as for
    /// [`Db::pinned`]; `None` for a column no such factor fixes.
    fn pinned_cols(&self, body: &UTerm, id: u32, env: &Env) -> Option<Vec<Option<UConst>>> {
        let width = *self.widths.get(&id)?;
        let factors: Vec<&UTerm> = match body {
            UTerm::Mul(fs) => fs.iter().map(|f| &**f).collect(),
            other => vec![other],
        };
        let mut cols: Vec<Option<UConst>> = vec![None; width];
        let mut env = env.clone();
        for f in factors {
            let UTerm::Pred { kind: PredKind::Eq, args } = f else { continue };
            let [a, b] = args.as_slice() else { continue };
            for (side, other) in [(a, b), (b, a)] {
                if let UTerm::Var(UVar::Proj { index, base }) = side {
                    if **base == UVar::Base(id) && cols.get(*index as usize).is_some_and(Option::is_none) {
                        if let Ok(v) = self.eval(other, &mut env) {
                            cols[*index as usize] = Some(v);
                        }
                    }
                }
            }
        }
        Some(cols)
    }

    /// Evaluates a term in multiplicity position.
    pub fn count(&self, t: &UTerm, env: &mut Env) -> Result<i64, String> {
        match self.eval(t, env)? {
            UConst::Int(n) => Ok(n),
            other => Err(format!("expected a multiplicity, got {other:?}")),
        }
    }

    fn var(&self, v: &UVar, env: &Env) -> Result<UConst, String> {
        match v {
            UVar::Proj { index, base } => {
                let row = self.tuple(base, env)?;
                row.get(*index as usize).cloned().ok_or_else(|| format!("column {index} out of range"))
            }
            UVar::Base(id) => Err(format!("whole-tuple var Base({id}) in value position")),
        }
    }

    fn tuple(&self, v: &UVar, env: &Env) -> Result<Vec<UConst>, String> {
        match v {
            UVar::Base(id) => env.get(id).cloned().ok_or_else(|| format!("unbound Base({id})")),
            UVar::Proj { .. } => Err("tuple of a Proj var".into()),
        }
    }

    fn sum(&self, vars: &[UVar], body: &UTerm, env: &mut Env) -> Result<i64, String> {
        // Σ_x Σ_y f = Σ_{x,y} f, and Σ_v (a + b) = Σ_v a + Σ_v b: both exact, and both give the
        // variable choice below more to work with.
        match body {
            UTerm::Sum { vars: inner, body } => {
                let mut all = vars.to_vec();
                all.extend(inner.iter().cloned());
                return self.sum(&all, body, env);
            }
            UTerm::Add(ts) if !vars.is_empty() => {
                let mut acc = 0i64;
                for t in ts {
                    acc = acc.checked_add(self.sum(vars, t, env)?).ok_or("overflow")?;
                }
                return Ok(acc);
            }
            _ => {}
        }
        if vars.is_empty() {
            return self.count(body, env);
        }
        // Enumerate a table-guarded var first, then a var its equalities pin to one tuple, and only
        // then fall back to the whole universe.
        let pick = vars
            .iter()
            .position(|v| matches!(v, UVar::Base(id) if table_guard(body, *id).is_some()))
            .or_else(|| vars.iter().position(|v| matches!(v, UVar::Base(id) if self.pinned(body, *id, env).is_some())))
            .unwrap_or(0);
        let mut rest = vars.to_vec();
        let first = rest.remove(pick);
        let UVar::Base(id) = &first else { return Err("Sum over a Proj var".into()) };
        let width = *self.widths.get(id).ok_or_else(|| format!("no width for Base({id})"))?;
        // A direct `Table` factor on this var zeroes every tuple not in that table, so its rows are
        // the only candidates -- exact, and what keeps wide tables evaluable. Likewise a var whose
        // every column a direct equality pins has one candidate.
        let candidates: Vec<Vec<UConst>> = match table_guard(body, *id) {
            Some(name) => {
                // Distinct by identity, so each tuple is visited once and `Table` supplies its count.
                let mut rows: Vec<Vec<UConst>> = Vec::new();
                for r in self.tables.get(name).into_iter().flatten() {
                    if !rows.iter().any(|x| x.len() == r.len() && x.iter().zip(r).all(|(a, b)| identical(a, b))) {
                        rows.push(r.clone());
                    }
                }
                rows
            }
            // Columns a direct equality pins take that value alone; only the others range over the
            // universe. A pinned value may lie outside the universe (a count, say), so enumerating it
            // there would miss the one tuple that counts.
            None => {
                let pins = self.pinned_cols(body, *id, env).unwrap_or_else(|| vec![None; width]);
                let free: Vec<usize> = (0..width).filter(|&i| pins[i].is_none()).collect();
                let n = self.universe.len();
                let total = n.checked_pow(free.len() as u32).filter(|&t| t <= MAX_ASSIGNMENTS).ok_or("universe too large")?;
                (0..total)
                    .map(|code| {
                        let mut tuple = pins.clone();
                        for (k, &i) in free.iter().enumerate() {
                            tuple[i] = Some(self.universe[(code / n.pow(k as u32)) % n].clone());
                        }
                        tuple.into_iter().map(|c| c.expect("every column filled")).collect()
                    })
                    .collect()
            }
        };
        let saved = env.get(id).cloned();
        let mut acc = 0i64;
        for tuple in candidates {
            env.insert(*id, tuple);
            acc = acc.checked_add(self.sum(&rest, body, env)?).ok_or("overflow")?;
        }
        match saved {
            Some(s) => env.insert(*id, s),
            None => env.remove(id),
        };
        Ok(acc)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::rc::Rc;

    #[test]
    fn a_column_pinned_outside_the_universe_is_still_counted() {
        // Σ_x [x.0 = 7] over two-column tuples: one tuple per value of x.1, though 7 is not in the
        // universe.
        let pin = UTerm::Pred {
            kind: PredKind::Eq,
            args: vec![UTerm::Var(UVar::proj(0, UVar::Base(0))), UTerm::Const(UConst::Int(7))],
        };
        let term = UTerm::Sum { vars: vec![UVar::Base(0)], body: Rc::new(UTerm::Mul(vec![Rc::new(pin)])) };
        let db = Db::new(HashMap::new(), vec![UConst::Int(0), UConst::Int(1)], HashMap::from([(0, 2)]));
        assert_eq!(db.count(&term, &mut Env::new()), Ok(2));
    }
}
