// Unless explicitly stated otherwise all files in this repository are licensed under the
// Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/).
// Copyright 2026-Present Datadog, Inc.

//! Which relations return at most one row, read off the IR, and what that makes of a row slice.
//!
//! A `LIMIT 1` over a query that cannot return two rows keeps every row it gets, so the slice is
//! the identity there: `SELECT b FROM t WHERE k = $1` and the same query with `LIMIT 1` are one
//! query when `k` is a key of `t`. [`slice()`] says when a slice can be read that way, and [`shape`]
//! is the analysis it asks.
//!
//! The claims hold on every database that satisfies the schemas' integrity constraints, which is
//! what the constraint rewrites (`ic`, `normalize::Normalizer::with_ics`) already assume. A unique
//! key whose columns are all NOT NULL ([`Schema::enforced_keys`]) is the only source of a bound:
//! no two rows of its table agree on it. A conjunct of a `WHERE` or of an inner join's `ON` that
//! equates a column with a value constant across the rows (a literal, a parameter, a cast of one,
//! a column of an enclosing query) holds that column to one value in every row the conjunct
//! passes, provided `=` on its type is identity of the values; two columns equated that way are
//! one value. A table whose key is held that way contributes at most one row. Nothing else is
//! read: an `OR`, an order comparison, a function call or a subquery holds nothing, so the
//! analysis can only miss a bound, never invent one.

use std::collections::BTreeSet;

use crate::ir::{Expr, JoinKind, Relation, Schema, Type};

/// What a slice does to its source: nothing ([`Slice::Identity`]), remove every row
/// ([`Slice::Empty`]), or something that depends on the rows and their order ([`Slice::Opaque`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Slice {
    Identity,
    Empty,
    Opaque,
}

/// The effect of `ORDER BY … LIMIT limit OFFSET offset` over `source`, which sits at level `base`.
/// Only literal counts are read: a parameter may be bound to 0, or to more rows than there are.
pub fn slice(source: &Relation, offset: Option<&Expr>, limit: Option<&Expr>, base: usize, schemas: &[Schema]) -> Slice {
    let limit = limit.map(literal_count);
    if limit == Some(Some(0)) {
        return Slice::Empty;
    }
    if offset.is_some_and(|o| literal_count(o) != Some(0)) {
        return Slice::Opaque;
    }
    match limit {
        None => Slice::Identity,
        Some(Some(n)) if n >= 1 && shape(source, base, schemas).max_one() => Slice::Identity,
        Some(_) => Slice::Opaque,
    }
}

/// A literal row count, as `LIMIT`/`OFFSET` carry one. `None` for anything else, `LIMIT NULL` and a
/// parameter included, and for a negative count, which Postgres rejects.
fn literal_count(e: &Expr) -> Option<i64> {
    match e {
        Expr::Literal { value, ty: Type::Integer } => value.parse::<i64>().ok().filter(|n| *n >= 0),
        _ => None,
    }
}

/// `query` with the slices removed that cannot make it empty when it is not: a literal `LIMIT` of
/// one or more with no `OFFSET`, at its top or under a projection or deduplication, which keep
/// emptiness as well. `None` when there is none. For an `EXISTS`, which reads only whether its
/// query returns a row.
pub fn strip_nonempty_slices(query: &Relation) -> Option<Relation> {
    match query {
        Relation::Sort { source, offset, limit, .. }
            if offset.as_ref().is_none_or(|o| literal_count(o) == Some(0))
                && limit.as_ref().and_then(literal_count).is_some_and(|n| n >= 1) =>
        {
            Some(strip_nonempty_slices(source).unwrap_or_else(|| (**source).clone()))
        }
        Relation::Project { source, target } => {
            strip_nonempty_slices(source).map(|s| Relation::Project { source: Box::new(s), target: target.clone() })
        }
        Relation::Distinct(source) => strip_nonempty_slices(source).map(|s| Relation::Distinct(Box::new(s))),
        _ => None,
    }
}

/// What [`shape`] knows about a relation's rows. Positions are 0-based in its output.
#[derive(Debug, Clone)]
pub struct Shape {
    /// The relation's rows are a sub-bag of the product of these: one per scanned input of an inner
    /// join tree, one for any other relation.
    comps: Vec<Comp>,
    /// Positions that hold one value in every row.
    consts: BTreeSet<usize>,
    /// Pairs of positions that hold the same value in every row, kept while the rows are still a
    /// product of `comps` so that a filter above an inner join can use the join's equalities.
    equal: Vec<(usize, usize)>,
    /// Per position: whether SQL's `=` on it is identity of the values, so that equating it with a
    /// constant holds it to one value. Its length is the relation's width.
    ident: Vec<bool>,
}

#[derive(Debug, Clone)]
struct Comp {
    /// The positions it supplies.
    cols: BTreeSet<usize>,
    /// Position sets no two of its rows agree on.
    keys: Vec<BTreeSet<usize>>,
    /// It contributes at most one row.
    max_one: bool,
}

/// How many key combinations [`Shape::keys`] builds before it stops; a bound, not a tuning knob.
const MAX_KEYS: usize = 8;

impl Shape {
    /// The relation returns at most one row.
    pub fn max_one(&self) -> bool {
        self.comps.iter().all(|c| c.max_one)
    }

    /// Position sets no two rows of the relation agree on: one key of every component that may
    /// contribute several rows.
    pub fn keys(&self) -> Vec<BTreeSet<usize>> {
        let mut acc = vec![BTreeSet::new()];
        for c in self.comps.iter().filter(|c| !c.max_one) {
            let mut next = Vec::new();
            for a in &acc {
                for k in &c.keys {
                    if next.len() < MAX_KEYS {
                        next.push(a.union(k).copied().collect());
                    }
                }
            }
            acc = next;
        }
        acc
    }

    fn width(&self) -> usize {
        self.ident.len()
    }

    /// One component over all positions, for a relation whose rows are no longer a product.
    fn collapsed(width: usize, keys: Vec<BTreeSet<usize>>, max_one: bool, consts: BTreeSet<usize>, ident: Vec<bool>) -> Shape {
        let comp = Comp { cols: (0..width).collect(), keys, max_one };
        Shape { comps: vec![comp], consts, equal: Vec::new(), ident }.settled()
    }

    /// Nothing known: one component, no key.
    fn unknown(ident: Vec<bool>) -> Shape {
        Shape::collapsed(ident.len(), Vec::new(), false, BTreeSet::new(), ident)
    }

    /// A relation with at most one row holds one value in each position.
    fn settled(mut self) -> Shape {
        if self.max_one() {
            self.consts = (0..self.width()).collect();
        }
        self
    }

    /// `self` followed by `right`, as an inner join's input rows.
    fn concat(mut self, right: Shape) -> Shape {
        let w = self.width();
        let shift = |s: &BTreeSet<usize>| s.iter().map(|p| p + w).collect::<BTreeSet<usize>>();
        self.comps.extend(right.comps.into_iter().map(|c| Comp {
            cols: shift(&c.cols),
            keys: c.keys.iter().map(shift).collect(),
            max_one: c.max_one,
        }));
        self.consts.extend(right.consts.iter().map(|p| p + w));
        self.equal.extend(right.equal.iter().map(|(p, q)| (p + w, q + w)));
        self.ident.extend(right.ident);
        self
    }

    /// Restrict to the rows `condition` passes, at level `base`: hold each column its conjuncts
    /// equate with a constant, merge the columns they equate with each other, and close over the
    /// components whose key that holds.
    fn filtered(mut self, condition: &Expr, base: usize) -> Shape {
        let w = self.width();
        let mut class: Vec<usize> = (0..w).collect();
        for &(p, q) in &self.equal {
            union(&mut class, p, q);
        }
        let mut pinned = self.consts.clone();
        for c in conjuncts(condition) {
            let Expr::Call { operator, operand, .. } = c else { continue };
            let [a, b] = operand.as_slice() else { continue };
            if operator != "=" || a.ty() != b.ty() {
                continue;
            }
            // The column's own reading of `=` decides, since Postgres reads the other operand, of
            // the same IR type, at the column's type.
            match (self.local(a, base), self.local(b, base)) {
                (Some(p), Some(q)) if self.ident[p] && self.ident[q] => {
                    union(&mut class, p, q);
                    self.equal.push((p, q));
                }
                (Some(p), None) if self.ident[p] && row_constant(b, base) => {
                    pinned.insert(p);
                }
                (None, Some(q)) if self.ident[q] && row_constant(a, base) => {
                    pinned.insert(q);
                }
                _ => {}
            }
        }
        loop {
            let held: BTreeSet<usize> = pinned.iter().map(|p| find(&mut class, *p)).collect();
            pinned.extend((0..w).filter(|p| held.contains(&find(&mut class, *p))));
            let mut changed = false;
            for comp in self.comps.iter_mut().filter(|c| !c.max_one) {
                if comp.keys.iter().any(|k| k.is_subset(&pinned)) {
                    comp.max_one = true;
                    pinned.extend(comp.cols.iter().copied());
                    changed = true;
                }
            }
            if !changed {
                break;
            }
        }
        self.consts = pinned;
        self.settled()
    }

    /// The position a bare column reference at level `base` names in this relation, if any.
    fn local(&self, e: &Expr, base: usize) -> Option<usize> {
        match e {
            Expr::Column { index, .. } => (*index as usize).checked_sub(base).filter(|p| *p < self.width()),
            _ => None,
        }
    }
}

/// `e`'s top-level `AND` operands, flattened; `e` itself when it is not an `AND`.
fn conjuncts(e: &Expr) -> Vec<&Expr> {
    match e {
        Expr::Call { operator, operand, .. } if operator == "AND" => operand.iter().flat_map(conjuncts).collect(),
        other => vec![other],
    }
}

/// A value that is the same in every row at level `base`: a literal, a parameter, a column of an
/// enclosing query, or one of the frontend's casts or conversions of such a value. No other
/// function, so a volatile one (`random()`, `clock_timestamp()`) is never taken for a constant.
fn row_constant(e: &Expr, base: usize) -> bool {
    match e {
        Expr::Literal { .. } => true,
        Expr::Column { index, .. } => (*index as usize) < base,
        Expr::Cast { operand, .. } => row_constant(operand, base),
        Expr::Call { operator, operand, .. } => {
            let numbered = |prefix: &str| {
                operator.strip_prefix(prefix).is_some_and(|k| !k.is_empty() && k.bytes().all(|b| b.is_ascii_digit()))
            };
            (numbered("QP") || numbered("QCAST") || operator.starts_with("q_cast_") || operator.starts_with("q_conv_"))
                && operand.iter().all(|o| row_constant(o, base))
        }
        Expr::Subquery { .. } => false,
    }
}

fn find(class: &mut [usize], p: usize) -> usize {
    let mut root = p;
    while class[root] != root {
        root = class[root];
    }
    class[p] = root;
    root
}

fn union(class: &mut [usize], p: usize, q: usize) {
    let (rp, rq) = (find(class, p), find(class, q));
    class[rp] = rq;
}

/// Whether `=` on a value of IR type `ty` is identity of the values, as `translate::EqType::of`
/// reads it: every type but REAL, INTERVAL and the opaque VARBINARY.
fn identity_type(ty: Type) -> bool {
    !matches!(ty, Type::Real | Type::Interval | Type::Varbinary)
}

/// What is known about `r`'s rows, `r` sitting at level `base` (its own columns are `base..`).
pub fn shape(r: &Relation, base: usize, schemas: &[Schema]) -> Shape {
    match r {
        Relation::Scan(i) => {
            let Some(s) = schemas.get(*i) else { return Shape::unknown(Vec::new()) };
            let vouched = |c: usize| s.opaque_identity.get(c).copied().unwrap_or(false);
            let ident =
                s.types.iter().enumerate().map(|(c, ty)| identity_type(*ty) || (*ty == Type::Varbinary && vouched(c))).collect();
            let keys = s.enforced_keys().map(|k| k.iter().copied().collect()).collect();
            Shape::collapsed(s.types.len(), keys, false, BTreeSet::new(), ident)
        }
        Relation::Filter { source, condition } => shape(source, base, schemas).filtered(condition, base),
        Relation::Join { left, right, kind, condition } => {
            let (l, r) = (shape(left, base, schemas), shape(right, base, schemas));
            match kind {
                JoinKind::Inner => l.concat(r).filtered(condition, base),
                // Every row of the preserved side appears, matched or padded with NULLs, so the
                // other side's columns hold nothing constant; with that side at most one row,
                // each preserved row appears once.
                JoinKind::Left | JoinKind::Right => {
                    let (kept, other, offset) = if *kind == JoinKind::Left { (&l, &r, 0) } else { (&r, &l, l.width()) };
                    let keys = if other.max_one() {
                        kept.keys().iter().map(|k| k.iter().map(|p| p + offset).collect()).collect()
                    } else {
                        Vec::new()
                    };
                    let consts = kept.consts.iter().map(|p| p + offset).collect();
                    let mut ident = l.ident.clone();
                    ident.extend(r.ident.iter().copied());
                    Shape::collapsed(ident.len(), keys, l.max_one() && r.max_one(), consts, ident)
                }
                JoinKind::Full => {
                    let mut ident = l.ident.clone();
                    ident.extend(r.ident.iter().copied());
                    Shape::unknown(ident)
                }
            }
        }
        Relation::Project { source, target } => {
            let src = shape(source, base, schemas);
            // The position in the source each output column passes through bare, if it does.
            let bare: Vec<Option<usize>> = target.iter().map(|e| src.local(e, base)).collect();
            let ident = target
                .iter()
                .zip(&bare)
                .map(|(e, b)| match b {
                    Some(p) => src.ident[*p],
                    None => identity_type(e.ty()),
                })
                .collect();
            let consts = (0..target.len())
                .filter(|j| match bare[*j] {
                    Some(p) => src.consts.contains(&p),
                    None => row_constant(&target[*j], base),
                })
                .collect();
            let keys = src
                .keys()
                .into_iter()
                .filter_map(|k| k.iter().map(|p| bare.iter().position(|b| *b == Some(*p))).collect::<Option<BTreeSet<usize>>>())
                .collect();
            Shape::collapsed(target.len(), keys, src.max_one(), consts, ident)
        }
        Relation::Distinct(source) => {
            let src = shape(source, base, schemas);
            let mut keys = src.keys();
            keys.push((0..src.width()).collect());
            Shape::collapsed(src.width(), keys, src.max_one(), src.consts.clone(), src.ident.clone())
        }
        Relation::Group { source, keys, function } => {
            let src = shape(source, base, schemas);
            let Some(at) = keys.iter().map(|k| src.local(k, base)).collect::<Option<Vec<usize>>>() else {
                return Shape::unknown(Relation::output_types(r, schemas).iter().map(|t| t.is_some_and(identity_type)).collect());
            };
            let max_one = keys.is_empty() || src.max_one() || at.iter().all(|p| src.consts.contains(p));
            let consts = (0..at.len()).filter(|i| src.consts.contains(&at[*i])).collect();
            let ident = at.iter().map(|p| src.ident[*p]).chain(function.iter().map(|f| identity_type(f.ty))).collect();
            Shape::collapsed(at.len() + function.len(), vec![(0..at.len()).collect()], max_one, consts, ident)
        }
        // Any slice keeps a sub-bag of its source, which keeps every bound; `LIMIT 1` adds one.
        Relation::Sort { source, limit, .. } => {
            let src = shape(source, base, schemas);
            if limit.as_ref().and_then(literal_count).is_some_and(|n| n <= 1) {
                Shape::collapsed(src.width(), src.keys(), true, src.consts.clone(), src.ident.clone())
            } else {
                src
            }
        }
        Relation::Values { schema, content } => {
            let ident = schema.iter().map(|t| identity_type(*t)).collect();
            Shape::collapsed(schema.len(), Vec::new(), content.len() <= 1, BTreeSet::new(), ident)
        }
        Relation::Union(_) => Shape::unknown(Relation::output_types(r, schemas).iter().map(|t| t.is_some_and(identity_type)).collect()),
        // Deduplicated rows of the left side, or some of them.
        Relation::Except(sides) | Relation::Intersect(sides) => {
            let src = shape(&sides[0], base, schemas);
            let mut keys = src.keys();
            keys.push((0..src.width()).collect());
            Shape::collapsed(src.width(), keys, src.max_one(), src.consts.clone(), src.ident.clone())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::Input;
    use serde_json::{json, Value};

    /// `t(k INTEGER NOT NULL, a INTEGER, b INTEGER NOT NULL, c INTEGER NOT NULL)`, keys `(k)` and
    /// `(b, c)`; and `u(id INTEGER NOT NULL, t_k INTEGER)`, key `(id)`; and `n(x INTEGER)`, whose key
    /// is over a nullable column.
    fn schemas() -> Vec<Schema> {
        let input = json!({ "schemas": [
            { "name": "t", "types": ["INTEGER", "INTEGER", "INTEGER", "INTEGER"], "nullable": [false, true, false, false], "key": [[0], [2, 3]] },
            { "name": "u", "types": ["INTEGER", "INTEGER"], "nullable": [false, true], "key": [[0]] },
            { "name": "n", "types": ["INTEGER"], "nullable": [true], "key": [[0]] },
        ], "queries": [{ "scan": 0 }, { "scan": 0 }] });
        Input::parse(&input).unwrap().schemas
    }

    fn rel(v: Value) -> Relation {
        Relation::parse(&v, 3).unwrap()
    }

    fn col(i: u32) -> Value {
        json!({ "column": i, "type": "INTEGER" })
    }

    fn param(n: u32) -> Value {
        json!({ "operator": format!("QP{n}"), "type": "INTEGER", "operand": [{ "operator": "0", "operand": [], "type": "INTEGER" }] })
    }

    fn eq(a: Value, b: Value) -> Value {
        json!({ "operator": "=", "type": "BOOLEAN", "operand": [a, b] })
    }

    fn and(parts: Vec<Value>) -> Value {
        json!({ "operator": "AND", "type": "BOOLEAN", "operand": parts })
    }

    fn filter(source: Value, cond: Value) -> Value {
        json!({ "filter": { "source": source, "condition": cond } })
    }

    fn max_one(v: Value) -> bool {
        shape(&rel(v), 0, &schemas()).max_one()
    }

    #[test]
    fn a_key_held_to_a_parameter_bounds_the_rows() {
        assert!(max_one(filter(json!({ "scan": 0 }), eq(col(0), param(1)))));
        assert!(max_one(filter(json!({ "scan": 0 }), eq(param(1), col(0)))));
        // Through a projection that drops the key.
        assert!(max_one(json!({ "project": { "source": filter(json!({ "scan": 0 }), eq(col(0), param(1))), "target": [col(1)] } })));
        // Both columns of the composite key.
        assert!(max_one(filter(json!({ "scan": 0 }), and(vec![eq(col(2), param(1)), eq(col(3), param(2))]))));
    }

    #[test]
    fn anything_short_of_a_held_key_does_not() {
        assert!(!max_one(json!({ "scan": 0 })));
        // A column that is no key.
        assert!(!max_one(filter(json!({ "scan": 0 }), eq(col(1), param(1)))));
        // Half of the composite key.
        assert!(!max_one(filter(json!({ "scan": 0 }), eq(col(2), param(1)))));
        // A key over a nullable column holds nothing.
        assert!(!max_one(filter(json!({ "scan": 2 }), eq(col(0), param(1)))));
        // An order comparison, a disjunction, an equality with another column of the same row.
        let le = json!({ "operator": "<=", "type": "BOOLEAN", "operand": [col(0), param(1)] });
        assert!(!max_one(filter(json!({ "scan": 0 }), le)));
        let or = json!({ "operator": "OR", "type": "BOOLEAN", "operand": [eq(col(0), param(1)), eq(col(0), param(2))] });
        assert!(!max_one(filter(json!({ "scan": 0 }), or)));
        assert!(!max_one(filter(json!({ "scan": 0 }), eq(col(0), col(1)))));
        // A function of a parameter that is not one of the frontend's casts.
        let f = json!({ "operator": "ABS", "type": "INTEGER", "operand": [param(1)] });
        assert!(!max_one(filter(json!({ "scan": 0 }), eq(col(0), f))));
    }

    #[test]
    fn a_held_key_carries_through_an_inner_join() {
        // `u JOIN t ON t.k = u.t_k WHERE u.id = $1`: `u` is one row, so its `t_k` is one value,
        // which holds `t`'s key. `t`'s columns follow `u`'s.
        let join = json!({ "join": { "kind": "INNER", "left": { "scan": 1 }, "right": { "scan": 0 }, "condition": eq(col(2), col(1)) } });
        assert!(max_one(filter(join.clone(), eq(col(0), param(1)))));
        // Held on `t` alone, `u` may still match many rows.
        assert!(!max_one(filter(join, eq(col(2), param(1)))));
    }

    #[test]
    fn groups_and_slices() {
        let grouped = |source: Value, keys: Vec<Value>| json!({ "group": { "source": source, "keys": keys, "function": [] } });
        // A scalar aggregate has exactly one row; a group whose keys are all held has at most one.
        assert!(max_one(grouped(json!({ "scan": 0 }), vec![])));
        assert!(max_one(grouped(filter(json!({ "scan": 0 }), eq(col(1), param(1))), vec![col(1)])));
        assert!(!max_one(grouped(json!({ "scan": 0 }), vec![col(1)])));
        let page = |source: Value, n: &str| {
            json!({ "sort": { "source": source, "collation": [], "limit": { "operator": n, "operand": [], "type": "INTEGER" } } })
        };
        assert!(max_one(page(json!({ "scan": 0 }), "1")));
        assert!(!max_one(page(json!({ "scan": 0 }), "2")));
    }

    /// What the analysis claims holds on concrete databases that keep the keys: each relation is
    /// translated and evaluated on random small contents of `t` and `u`, and one it calls at most
    /// one row never has two.
    #[test]
    fn at_most_one_row_holds_on_databases_that_keep_the_keys() {
        use crate::eval::{Db, Env};
        use crate::translate::{translate_input, OUT_VAR_ID};
        use crate::uterm::{mk_sum, UConst, UVar};
        use std::collections::{HashMap, HashSet};

        let lit = |n: &str| json!({ "operator": n, "operand": [], "type": "INTEGER" });
        let scan = |i: u32| json!({ "scan": i });
        let join = json!({ "join": { "kind": "INNER", "left": scan(1), "right": scan(0), "condition": eq(col(2), col(1)) } });
        let grouped = |source: Value| json!({ "group": { "source": source, "keys": [col(1)], "function": [] } });
        let rels = [
            filter(scan(0), eq(col(0), param(1))),
            filter(scan(0), eq(col(0), lit("1"))),
            filter(scan(0), and(vec![eq(col(2), lit("1")), eq(col(3), param(2))])),
            filter(join.clone(), eq(col(0), param(1))),
            filter(join.clone(), eq(col(0), lit("2"))),
            filter(join.clone(), eq(col(2), lit("1"))),
            grouped(filter(scan(0), eq(col(1), lit("1")))),
            grouped(scan(0)),
            filter(scan(0), eq(col(2), lit("1"))),
            scan(1),
        ];
        let s = schemas();
        let universe = vec![UConst::Null, UConst::Int(0), UConst::Int(1), UConst::Int(2)];
        let mut seed: u64 = 7;
        let mut next = |m: u64| {
            seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            (seed >> 33) % m
        };
        let mut saw_many = false;
        for _ in 0..60 {
            let (mut t, mut ks, mut bcs) = (Vec::new(), HashSet::new(), HashSet::new());
            for _ in 0..next(6) {
                let (k, a, b, c) = (next(3) as i64, universe[next(4) as usize].clone(), next(3) as i64, next(3) as i64);
                if ks.insert(k) && bcs.insert((b, c)) {
                    t.push(vec![UConst::Int(k), a, UConst::Int(b), UConst::Int(c)]);
                }
            }
            let (mut u, mut ids) = (Vec::new(), HashSet::new());
            for _ in 0..next(5) {
                let id = next(3) as i64;
                if ids.insert(id) {
                    u.push(vec![UConst::Int(id), universe[next(4) as usize].clone()]);
                }
            }
            let tables: HashMap<String, Vec<Vec<UConst>>> = [("t".into(), t), ("u".into(), u), ("n".into(), vec![])].into();
            for v in &rels {
                let r = rel(v.clone());
                let claimed = shape(&r, 0, &s).max_one();
                let [q, _] = translate_input(&Input { schemas: s.clone(), queries: [r.clone(), r] }).unwrap();
                let db = Db::new(tables.clone(), universe.clone(), q.widths.clone());
                let rows = db.count(&mk_sum(vec![UVar::Base(OUT_VAR_ID)], q.term.clone()), &mut Env::new()).unwrap();
                assert!(!claimed || rows <= 1, "{v} has {rows} rows on {tables:?}");
                saw_many |= rows > 1;
            }
        }
        assert!(saw_many, "no relation ever had two rows, so the check above checked nothing");
    }

    #[test]
    fn a_slice_is_the_identity_only_where_it_keeps_every_row() {
        let lit = |n: &str| Expr::Literal { value: n.to_string(), ty: Type::Integer };
        let one_row = rel(filter(json!({ "scan": 0 }), eq(col(0), param(1))));
        let many = rel(json!({ "scan": 0 }));
        let s = schemas();
        assert_eq!(slice(&one_row, None, Some(&lit("1")), 0, &s), Slice::Identity);
        assert_eq!(slice(&one_row, Some(&lit("0")), Some(&lit("5")), 0, &s), Slice::Identity);
        assert_eq!(slice(&many, None, Some(&lit("0")), 0, &s), Slice::Empty);
        assert_eq!(slice(&many, Some(&lit("0")), None, 0, &s), Slice::Identity);
        assert_eq!(slice(&many, None, Some(&lit("1")), 0, &s), Slice::Opaque);
        assert_eq!(slice(&one_row, Some(&lit("1")), Some(&lit("1")), 0, &s), Slice::Opaque);
        let p = Expr::parse(&param(1), 3).unwrap();
        assert_eq!(slice(&one_row, None, Some(&p), 0, &s), Slice::Opaque);
    }
}
