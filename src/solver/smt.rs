// Unless explicitly stated otherwise all files in this repository are licensed under the
// Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/).
// Copyright 2026-Present Datadog, Inc.

//! The Z3 encoding of U-expressions, shared by the set solver (rung 3, `setsolver`) and the bag
//! congruence rung (`bagsolver`).
//!
//! * **One uninterpreted sort `Val`** for every value. Each constant is its own `Val` constant, one
//!   per value (`1`, `1.0` and `1.00` are three, see [`UConst::same_value`]), `Null` among them.
//!   Two constants that are certainly different values are asserted distinct: two different
//!   numbers, two different strings, a number and a string, `Null` and anything else. Equal numbers
//!   of different types or scales are left free to be one value or two, since neither holds in
//!   every place they meet. `Eq` is identity and `Ne` its negation.
//! * **An order comparison** is read by its [`Order`]:
//!   * In an `Opaque` one, `Lt` is an uninterpreted relation and `Le` its complement with the
//!     operands swapped (`a <= b` is `¬(b < a)`). That is the one order fact encoded there, and it
//!     holds wherever it is used: the translator puts every order comparison under its operands'
//!     not-null guard, and the non-NULL values of one type are totally ordered (preordered, under
//!     a collation that ties distinct strings, or a type whose `=` ties distinct values, as
//!     `numeric`'s does `2.0` and `2.00`).
//!   * In an `Integer`, `Text`, `Date`, `Time` or `Timestamp` one, each operand has a rank
//!     `ord:<order>: Val → Int`, and the comparison is the ranks' comparison, false when either
//!     operand is `Null` (as `eval` reads it). Every ranked term `t` gets the ground fact
//!     `ord⁻¹(ord(t)) = t`, so ranks are injective on them: a value that is neither less nor more
//!     than another is that value, which is antisymmetry under an `=` that is identity, the only
//!     kind these orders are given to (`translate::order_of`).
//!   * Only `Integer` ranks of integer constants are fixed, to the integer itself, so integers are
//!     compared exactly, and discretely. Unbounded `Int` is a superset of SQL's integers, so what
//!     holds over it holds over them. Every other rank is free, the INTEGER ranks of the dates and
//!     timestamps the IR also types INTEGER included (they reach a comparison as opaque terms, not
//!     integer constants), so text constants are not ordered: the collation decides that.
//!   * Free ranks assume no more than a total order. A quantifier-free formula over `<` and `=` is
//!     satisfiable in a total order iff it is in `Int`, since each finite configuration embeds,
//!     and nothing does arithmetic on a rank.
//! * **Functions and value-position arithmetic** are uninterpreted `Val` functions -- sound, since
//!   the real ones are among their interpretations, and ours already take (null flag, value) pairs.
//! * **A table** `T` of width `w` is an uninterpreted `Val^w → Int`: its value at a tuple is the
//!   tuple's multiplicity, read as `> 0` in set positions.
//! * **A base var** of width `w` is `w` `Val` constants: bound by an `∃` where its sum is in a set
//!   position, free (hence universally quantified by a validity question) everywhere else.
//!
//! Interpretations range over a superset of real databases (multiplicities may go negative in a
//! model), so UNSAT over them implies agreement on every real one; the extra models only cost
//! completeness.
//!
//! Three readings are opt-in ([`Encoder::interpreting`], [`Encoder::with_facts`],
//! [`Encoder::with_value_sums`]), each true of every real database, so each only narrows the models:
//! * **Facts about rows.** A row of a table satisfies the table's NOT NULL columns and the BOOLEAN
//!   domain (`0`, `1` or `Null`, the only values a boolean column holds). A table atom is encoded
//!   as `ite(facts(row), T(row), 0)`: equal to `T(row)` on every real database, and it carries the
//!   facts into every position the atom reaches, inside a quantifier included.
//! * **Selections in value positions.** The translator writes `CASE`, `COALESCE` and a value's NULL
//!   sentinel as a sum of products, each a 0/1 guard times one value, with guards that exclude each
//!   other. Such a sum is the value whose guard holds, so it is encoded as an `ite` chain, and that
//!   the guards exclude each other is recorded as an obligation ([`Encoder::obligations`]) the
//!   caller must prove alongside its goal. A product of guards and one value is that value where
//!   the guards hold and `0` elsewhere, exactly as `eval` reads it. And a multiplicity used as a
//!   value (a predicate in a boolean output column) is the integer constant it is.
//! * **Value-position sums** (aggregates, scalar subqueries): the caller names each one's class
//!   and the columns it reads; a sum is then an uninterpreted function of those columns, shared by
//!   every sum of its class.

use std::collections::{BTreeSet, HashMap};

use z3::ast::{self, Ast, Bool, Dynamic, Int};
use z3::{FuncDecl, Sort, Symbol};

use crate::solver::ir::{Schema, Type};
use crate::solver::normalize::is_zero_one;
use crate::solver::translate::OUT_VAR_ID;
use crate::solver::uterm::{Number, Order, PredKind, UConst, UTerm, UVar};

/// Which side of a pair a term comes from: their bound vars are distinct constants, the output var
/// is shared.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Side {
    A,
    B,
}

/// What every row of each table satisfies, by table name: its NOT NULL columns, and its BOOLEAN
/// columns, whose values are `0`, `1` or `Null`. The first is an integrity constraint, which every
/// claim of this crate already assumes (`ic`); the second is a fact about the type.
#[derive(Debug, Clone, Default)]
pub struct Facts {
    tables: HashMap<String, TableFacts>,
}

#[derive(Debug, Clone, Default)]
struct TableFacts {
    not_null: Vec<usize>,
    boolean: Vec<usize>,
}

impl Facts {
    pub fn from_schemas(schemas: &[Schema]) -> Facts {
        let tables = schemas
            .iter()
            .map(|s| {
                let not_null = (0..s.types.len()).filter(|c| s.nullable.get(*c) == Some(&false)).collect();
                let boolean = s.types.iter().enumerate().filter(|(_, t)| **t == Type::Boolean).map(|(c, _)| c).collect();
                (s.name.clone(), TableFacts { not_null, boolean })
            })
            .collect();
        Facts { tables }
    }
}

/// The class of values a constant certainly belongs to, such that two constants in different classes
/// are certainly different values: its exact number for an INTEGER or REAL (so `1`, `1.0` and
/// `1.00` share one), its text for a string, NULL alone. `None` when there is no exact reading, and
/// then the constant is left out of every distinctness assertion, since declaring two equal values
/// distinct would let the solver prove false equalities.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) enum ValueClass {
    Number(Number),
    Str(String),
    Null,
}

pub(crate) fn value_class(c: &UConst) -> Option<ValueClass> {
    match c {
        UConst::Int(_) | UConst::Decimal(_) => c.number().map(ValueClass::Number),
        UConst::Str(s) => Some(ValueClass::Str(s.clone())),
        UConst::Null => Some(ValueClass::Null),
    }
}

/// A value-position sum's class and the columns it reads (each a `Var(Proj)`), as the caller of
/// [`Encoder::with_value_sums`] names them.
pub type ValueSums = HashMap<UTerm, (String, Vec<UTerm>)>;

pub struct Encoder {
    val: Sort,
    /// One `Val` constant per value (`UConst` is its own identity, see [`UConst::same_value`]), with
    /// its value class.
    consts: HashMap<UConst, (Dynamic, Option<ValueClass>)>,
    /// The `Val` constants standing for each base var's columns. The output var is shared by both
    /// sides; every other var belongs to one side.
    vars: HashMap<(Option<Side>, u32), Vec<Dynamic>>,
    funcs: HashMap<String, FuncDecl>,
    interpret: bool,
    facts: Facts,
    value_sums: ValueSums,
    obligations: Vec<Bool>,
    /// The integers a multiplicity was used as a value for, to link to their constants.
    ints: BTreeSet<i64>,
    /// The terms an interpreted order comparison ranked, for their injectivity facts, and the
    /// integer constants an `Integer` one ranked, for their fixed ranks (see [`Encoder::pred`]).
    ranked: Vec<(Order, Dynamic)>,
    ranked_ints: BTreeSet<i64>,
}

impl Default for Encoder {
    fn default() -> Self {
        Self::new()
    }
}

impl Encoder {
    pub fn new() -> Self {
        Encoder {
            val: Sort::uninterpreted(Symbol::String("Val".into())),
            consts: HashMap::new(),
            vars: HashMap::new(),
            funcs: HashMap::new(),
            interpret: false,
            facts: Facts::default(),
            value_sums: HashMap::new(),
            obligations: Vec::new(),
            ints: BTreeSet::new(),
            ranked: Vec::new(),
            ranked_ints: BTreeSet::new(),
        }
    }

    /// Read selections and multiplicities in value positions (see the module doc).
    pub fn interpreting(mut self) -> Self {
        self.interpret = true;
        self
    }

    pub fn with_facts(mut self, facts: Facts) -> Self {
        self.facts = facts;
        self
    }

    pub fn with_value_sums(mut self, sums: ValueSums) -> Self {
        self.value_sums = sums;
        self
    }

    /// What the encoding claimed while reading selections: that their guards exclude each other.
    pub fn obligations(&self) -> Vec<Bool> {
        self.obligations.clone()
    }

    /// That constants of different value classes are different values, as `class(k) = i` with one
    /// integer `i` per class: congruence then keeps every two classes apart, and leaves the members
    /// of one class free. With values interpreted, also that each integer a multiplicity was used as
    /// is that integer's constant.
    pub fn background(&mut self) -> Vec<Bool> {
        let mut facts = self.order_facts();
        if self.interpret {
            let int_sort = Sort::int();
            let val = self.val.clone();
            for n in self.ints.clone() {
                let k = self.constant(&UConst::Int(n));
                let image = self.apply("int2val".to_string(), &[&int_sort], &val, &[&Int::from_i64(n)]);
                facts.push(image.eq(&k));
            }
        }
        let mut index: HashMap<&ValueClass, i64> = HashMap::new();
        for (_, class) in self.consts.values() {
            if let Some(c) = class {
                let next = index.len() as i64;
                index.entry(c).or_insert(next);
            }
        }
        if index.len() < 2 {
            return facts;
        }
        let class_of = FuncDecl::new("value-class", &[&self.val], &Sort::int());
        facts.extend(
            self.consts
                .values()
                .filter_map(|(k, class)| Some(class_of.apply(&[k]).as_int()?.eq(Int::from_i64(index[class.as_ref()?])))),
        );
        facts
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

    pub(crate) fn constant(&mut self, c: &UConst) -> Dynamic {
        let val = &self.val;
        self.consts.entry(c.clone()).or_insert_with(|| (Dynamic::fresh_const("k", val), value_class(c))).0.clone()
    }

    fn table(&mut self, name: &str, var: &UVar, side: Side, widths: &HashMap<u32, usize>) -> Option<Int> {
        let UVar::Base(id) = var else { return None };
        let cols = self.columns(side, *id, widths)?;
        let domain: Vec<Sort> = vec![self.val.clone(); cols.len()];
        let refs: Vec<&Sort> = domain.iter().collect();
        let args: Vec<&dyn Ast> = cols.iter().map(|c| c as &dyn Ast).collect();
        let count = self.apply(format!("T:{name}/{}", cols.len()), &refs, &Sort::int(), &args).as_int()?;
        let Some(tf) = self.facts.tables.get(name).cloned() else { return Some(count) };
        let null = self.constant(&UConst::Null);
        let (zero, one) = (self.constant(&UConst::Int(0)), self.constant(&UConst::Int(1)));
        let mut holds: Vec<Bool> = tf.not_null.iter().filter_map(|c| cols.get(*c)).map(|v| v.eq(&null).not()).collect();
        holds.extend(tf.boolean.iter().filter_map(|c| cols.get(*c)).map(|v| Bool::or(&[v.eq(&zero), v.eq(&one), v.eq(&null)])));
        if holds.is_empty() {
            return Some(count);
        }
        Some(Bool::and(&holds).ite(&count, &Int::from_i64(0)))
    }

    /// A multiplicity position.
    pub fn count(&mut self, t: &UTerm, side: Side, widths: &HashMap<u32, usize>) -> Option<Int> {
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
    pub fn set(&mut self, t: &UTerm, side: Side, widths: &HashMap<u32, usize>) -> Option<Bool> {
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
    pub fn value(&mut self, t: &UTerm, side: Side, widths: &HashMap<u32, usize>) -> Option<Dynamic> {
        if self.interpret {
            if let Some(v) = self.selection(t, side, widths) {
                return v;
            }
        }
        Some(match t {
            UTerm::Const(c) => self.constant(c),
            UTerm::Var(UVar::Proj { index, base }) => {
                let UVar::Base(id) = **base else { return None };
                self.columns(side, id, widths)?.get(*index as usize)?.clone()
            }
            UTerm::Func { name, args } => self.apply_val(format!("f:{name}"), args, side, widths)?,
            UTerm::Add(ts) => self.apply_val("v:+".to_string(), &ts.iter().map(|c| (**c).clone()).collect::<Vec<_>>(), side, widths)?,
            UTerm::Mul(ts) => self.apply_val("v:*".to_string(), &ts.iter().map(|c| (**c).clone()).collect::<Vec<_>>(), side, widths)?,
            // A multiplicity used as a value: some fixed injection of the integer into `Val`, which
            // with values interpreted is the integer's own constant (`background`).
            UTerm::Pred { .. } | UTerm::Squash(_) | UTerm::Neg(_) | UTerm::Table { .. } => {
                let n = self.count(t, side, widths)?;
                if self.interpret {
                    self.ints.extend([0, 1]);
                }
                let (int_sort, val) = (Sort::int(), self.val.clone());
                self.apply("int2val".to_string(), &[&int_sort], &val, &[&n])
            }
            UTerm::Sum { .. } => {
                let (class, reads) = self.value_sums.get(t)?.clone();
                self.apply_val(format!("s:{class}"), &reads, side, widths)?
            }
            UTerm::Var(UVar::Base(_)) => return None,
        })
    }

    /// A sum or product read as a selection (see the module doc), or `None` when `t` is not one.
    /// The outer `Option` is whether it is a selection, the inner whether it encodes.
    fn selection(&mut self, t: &UTerm, side: Side, widths: &HashMap<u32, usize>) -> Option<Option<Dynamic>> {
        let branch = |t: &UTerm| -> Option<(Vec<UTerm>, Option<UTerm>)> {
            let factors: Vec<UTerm> = match t {
                UTerm::Mul(fs) => fs.iter().map(|f| (**f).clone()).collect(),
                other => vec![other.clone()],
            };
            let (guards, values): (Vec<UTerm>, Vec<UTerm>) = factors.into_iter().partition(is_zero_one);
            (!guards.is_empty() && values.len() <= 1).then(|| (guards, values.into_iter().next()))
        };
        let branches: Vec<(Vec<UTerm>, Option<UTerm>)> = match t {
            UTerm::Add(ts) => ts.iter().map(|c| branch(c)).collect::<Option<_>>()?,
            UTerm::Mul(_) => vec![branch(t)?],
            _ => return None,
        };
        let encode = |enc: &mut Self| -> Option<Dynamic> {
            let mut guards = Vec::with_capacity(branches.len());
            let mut values = Vec::with_capacity(branches.len());
            for (g, v) in &branches {
                let parts: Vec<Bool> = g.iter().map(|f| enc.set(f, side, widths)).collect::<Option<_>>()?;
                guards.push(Bool::and(&parts));
                values.push(match v {
                    Some(v) => enc.value(v, side, widths)?,
                    None => enc.constant(&UConst::Int(1)),
                });
            }
            for i in 0..guards.len() {
                for j in i + 1..guards.len() {
                    enc.obligations.push(Bool::and(&[&guards[i], &guards[j]]).not());
                }
            }
            let mut out = enc.constant(&UConst::Int(0));
            for (g, v) in guards.iter().zip(&values).rev() {
                out = g.ite(v, &out);
            }
            Some(out)
        };
        Some(encode(self))
    }

    fn apply_val(&mut self, name: String, args: &[UTerm], side: Side, widths: &HashMap<u32, usize>) -> Option<Dynamic> {
        let vals: Vec<Dynamic> = args.iter().map(|a| self.value(a, side, widths)).collect::<Option<_>>()?;
        let domain: Vec<Sort> = vec![self.val.clone(); vals.len()];
        let refs: Vec<&Sort> = domain.iter().collect();
        let range = self.val.clone();
        let args: Vec<&dyn Ast> = vals.iter().map(|v| v as &dyn Ast).collect();
        Some(self.apply(format!("{name}/{}", vals.len()), &refs, &range, &args))
    }

    /// `t`'s rank in `order` (see the module doc), recording it for [`Encoder::order_facts`].
    fn rank(&mut self, order: Order, t: &UTerm, v: &Dynamic) -> Option<Int> {
        if let (Order::Integer, UTerm::Const(UConst::Int(n))) = (order, t) {
            self.ranked_ints.insert(*n);
        }
        if !self.ranked.iter().any(|(o, r)| *o == order && r == v) {
            self.ranked.push((order, v.clone()));
        }
        let val = self.val.clone();
        self.apply(format!("ord:{order:?}"), &[&val], &Sort::int(), &[v]).as_int()
    }

    /// Injectivity of every rank taken, as `ord⁻¹(ord(t)) = t` for each ranked term `t`, and each
    /// ranked integer constant's rank, the integer itself.
    fn order_facts(&mut self) -> Vec<Bool> {
        let (val, int) = (self.val.clone(), Sort::int());
        let mut facts = Vec::new();
        for (order, t) in self.ranked.clone() {
            let rank = self.apply(format!("ord:{order:?}"), &[&val], &int, &[&t]);
            facts.push(self.apply(format!("ord-inverse:{order:?}"), &[&int], &val, &[&rank]).eq(&t));
        }
        for n in self.ranked_ints.clone() {
            let k = self.constant(&UConst::Int(n));
            if let Some(rank) = self.apply("ord:Integer".to_string(), &[&val], &int, &[&k]).as_int() {
                facts.push(rank.eq(Int::from_i64(n)));
            }
        }
        facts
    }

    pub fn pred(&mut self, kind: PredKind, args: &[UTerm], side: Side, widths: &HashMap<u32, usize>) -> Option<Bool> {
        let [a, b] = args else { return None };
        let (va, vb) = (self.value(a, side, widths)?, self.value(b, side, widths)?);
        if let Some(order) = kind.order().filter(|o| *o != Order::Opaque) {
            let (ra, rb) = (self.rank(order, a, &va)?, self.rank(order, b, &vb)?);
            let null = self.constant(&UConst::Null);
            let holds = match kind {
                PredKind::Lt(_) => ra.lt(&rb),
                PredKind::Le(_) => ra.le(&rb),
                PredKind::Gt(_) => ra.gt(&rb),
                PredKind::Ge(_) => ra.ge(&rb),
                PredKind::Eq | PredKind::Ne => unreachable!("an order kind"),
            };
            return Some(Bool::and(&[va.eq(&null).not(), vb.eq(&null).not(), holds]));
        }
        let rel = |enc: &mut Self, name: &str, x: &Dynamic, y: &Dynamic| -> Option<Bool> {
            let (val, boolean) = (enc.val.clone(), Sort::bool());
            enc.apply(name.to_string(), &[&val, &val], &boolean, &[x, y]).as_bool()
        };
        match kind {
            PredKind::Eq => Some(va.eq(&vb)),
            PredKind::Ne => Some(va.eq(&vb).not()),
            PredKind::Lt(_) => rel(self, "lt", &va, &vb),
            PredKind::Le(_) => rel(self, "lt", &vb, &va).map(|lt| lt.not()),
            PredKind::Gt(_) => rel(self, "lt", &vb, &va),
            PredKind::Ge(_) => rel(self, "lt", &va, &vb).map(|lt| lt.not()),
        }
    }
}

/// The columns `t` reads of vars it does not bind itself, each as a `Var(Proj)`, in a fixed order;
/// a table atom over such a var reads all of its columns.
pub fn free_columns(t: &UTerm, widths: &HashMap<u32, usize>) -> Vec<UTerm> {
    fn base(v: &UVar) -> Option<u32> {
        match v {
            UVar::Base(id) => Some(*id),
            UVar::Proj { base: inner, .. } => base(inner),
        }
    }
    fn go(t: &UTerm, bound: &mut Vec<u32>, widths: &HashMap<u32, usize>, out: &mut BTreeSet<(u32, u32)>) {
        match t {
            UTerm::Const(_) => {}
            UTerm::Var(UVar::Proj { index, base: b }) => {
                if let Some(id) = base(b) {
                    if !bound.contains(&id) {
                        out.insert((id, *index));
                    }
                }
            }
            UTerm::Var(UVar::Base(_)) => {}
            UTerm::Table { var, .. } => {
                if let Some(id) = base(var) {
                    if !bound.contains(&id) {
                        let w = widths.get(&id).copied().unwrap_or(0) as u32;
                        out.extend((0..w).map(|i| (id, i)));
                    }
                }
            }
            UTerm::Pred { args, .. } | UTerm::Func { args, .. } => args.iter().for_each(|a| go(a, bound, widths, out)),
            UTerm::Add(ts) | UTerm::Mul(ts) => ts.iter().for_each(|c| go(c, bound, widths, out)),
            UTerm::Squash(c) | UTerm::Neg(c) => go(c, bound, widths, out),
            UTerm::Sum { vars, body } => {
                let n = bound.len();
                bound.extend(vars.iter().filter_map(base));
                go(body, bound, widths, out);
                bound.truncate(n);
            }
        }
    }
    let mut out = BTreeSet::new();
    go(t, &mut Vec::new(), widths, &mut out);
    out.into_iter().map(|(id, i)| UTerm::Var(UVar::proj(i, UVar::Base(id)))).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn equal_numbers_share_a_value_class_but_not_a_constant() {
        // `2` and `2.0` are equal numbers, so they are never asserted distinct, but they are two
        // values: a cast or a division tells them apart.
        let (two, two_dec) = (UConst::Int(2), UConst::decimal("2.0").unwrap());
        assert_eq!(value_class(&two), value_class(&two_dec));
        assert_eq!(value_class(&UConst::decimal("001.500").unwrap()), value_class(&UConst::decimal("1.5").unwrap()));
        assert_ne!(value_class(&two), value_class(&UConst::decimal("2.5").unwrap()));
        let mut enc = Encoder::new();
        assert_ne!(enc.constant(&two), enc.constant(&two_dec));
        assert_eq!(enc.constant(&two), enc.constant(&UConst::Int(2)));
    }
}
