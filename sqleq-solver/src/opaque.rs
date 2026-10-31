// Unless explicitly stated otherwise all files in this repository are licensed under the
// Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/).
// Copyright 2026-Present Datadog, Inc.

//! Congruence for the nodes translation cannot read: the same slice, or the same opaque aggregate,
//! of equal sources on the two sides is one relation, whatever it computes.
//!
//! Two nodes are refused by translation however they stand: a row slice that keeps some of its
//! rows and not others (`ORDER BY … LIMIT $1`, which `card::slice` cannot read), and a `GROUP BY`
//! whose aggregate translation does not model (the frontend's `DISTINCT_ON#…` carriers, `SUM` of
//! the distinct values). Where one stands on each side with the same specification — the same sort
//! keys, counts, group keys and calls — and the two sources are proved to return the same bag, the
//! two nodes return the same rows, so both are replaced by a scan of one fresh table and the rest
//! of the pair is proved over it. A proof over a table that may hold anything holds in particular
//! for the rows the nodes return.
//!
//! "The same rows" rests on reading a slice as a function of its source's bag, which real SQL does
//! not promise: rows the ordering ties are kept in whatever order the plan reads them. That is the
//! reading QED's opaque `limit`, the frontend's own `strip_identical_pagination` and its lowering
//! (`lower::apply_pagination`) already take, and this module takes nothing beyond it. The frontend
//! keeps it honest where a slice could see more than its source's bag: an order key the query does
//! not output is added to the slice's source (`sort_sandwich`), so the sources compared here carry
//! every key; a slice or `DISTINCT ON` over a subquery whose `ORDER BY` lowering drops is refused
//! there; and so is a slice ordered by a string where a column declares a collation.
//!
//! A node that reads a column of an enclosing query is left alone (its rows differ from one outer
//! row to the next), and so is a pair in which any refused node finds no partner: translation then
//! refuses it as before. Matching is one to one, so two nodes on one side never share a table.

use crate::card::{self, Slice};
use crate::ir::{AggCall, Expr, Input, Relation, Schema};
use crate::prove::{prove_at, Verdict};

/// How many sub-proofs deep a source comparison may nest (a slice inside a slice's source).
pub const MAX_DEPTH: usize = 3;

/// More refused nodes than this on one side and the pair is left to translation to refuse.
const MAX_NODES: usize = 8;

/// `input` with each refused node replaced by a scan of a fresh table shared with its partner on the
/// other side, when every refused node has one. `None` when there is no refused node, or some node
/// is correlated or has no partner. `depth` is how many sub-proofs deep this pair already is.
pub fn abstract_opaque(input: &Input, depth: usize) -> Option<Input> {
    let schemas = &input.schemas;
    let nodes = |q: &Relation| {
        let mut found = Vec::new();
        each_node(&mut q.clone(), 0, schemas, &mut |r, base| {
            if !refused(r, base, schemas) {
                return false;
            }
            found.push(rebased(r, base));
            true
        });
        found
    };
    let (left, right) = (nodes(&input.queries[0]), nodes(&input.queries[1]));
    if left.is_empty() || left.len() != right.len() || left.len() > MAX_NODES {
        return None;
    }
    let left: Vec<Relation> = left.into_iter().collect::<Option<_>>()?;
    let right: Vec<Relation> = right.into_iter().collect::<Option<_>>()?;

    // `partner[j]`: the left node right node `j` is matched with.
    let mut partner: Vec<Option<usize>> = vec![None; right.len()];
    for (i, l) in left.iter().enumerate() {
        let j = (0..right.len()).find(|&j| partner[j].is_none() && congruent(l, &right[j], schemas, depth))?;
        partner[j] = Some(i);
    }

    let mut schemas = schemas.clone();
    let first = schemas.len();
    for (i, l) in left.iter().enumerate() {
        let types = l.output_types(&schemas).into_iter().collect::<Option<Vec<_>>>()?;
        let nullable = vec![true; types.len()];
        schemas.push(Schema { name: format!("#opaque{i}"), types, key: Vec::new(), nullable, opaque_identity: Vec::new() });
    }
    let replace = |q: &Relation, table: &dyn Fn(usize) -> usize| {
        let mut q = q.clone();
        let mut seen = 0;
        each_node(&mut q, 0, &schemas, &mut |r, base| {
            if !refused(r, base, &schemas) {
                return false;
            }
            *r = Relation::Scan(first + table(seen));
            seen += 1;
            true
        });
        q
    };
    let l = replace(&input.queries[0], &|i| i);
    let r = replace(&input.queries[1], &|j| partner[j].expect("every right node was matched above"));
    Some(Input { schemas, queries: [l, r] })
}

/// Whether translation refuses `r`, at level `base`, for being a node it cannot read.
fn refused(r: &Relation, base: usize, schemas: &[Schema]) -> bool {
    match r {
        Relation::Sort { source, offset, limit, .. } => {
            card::slice(source, offset.as_ref(), limit.as_ref(), base, schemas) == Slice::Opaque
        }
        Relation::Group { function, .. } => function.iter().any(|f| !translated_aggregate(f)),
        _ => false,
    }
}

/// The aggregate calls `translate::Translator::agg_eq` models, by name and distinctness.
fn translated_aggregate(f: &AggCall) -> bool {
    match f.operator.as_str() {
        "COUNT" | "MAX" | "MIN" => true,
        "SUM" | "AVG" => !f.distinct,
        _ => false,
    }
}

/// `r`, at level `base`, renumbered to stand alone at level 0; `None` when it reads a column of an
/// enclosing query. Every level inside it is at least `base`, so one shift renumbers all of them.
fn rebased(r: &Relation, base: usize) -> Option<Relation> {
    let mut r = r.clone();
    let mut correlated = false;
    each_expr(&mut r, &mut |e| {
        if let Expr::Column { index, .. } = e {
            match (*index as usize).checked_sub(base) {
                Some(i) => *index = i as u32,
                None => correlated = true,
            }
        }
    });
    (!correlated).then_some(r)
}

/// Whether two refused nodes, each standing alone at level 0, return the same rows: the same kind
/// and specification, output types known and equal, and sources proved to return the same bag.
fn congruent(l: &Relation, r: &Relation, schemas: &[Schema], depth: usize) -> bool {
    let (ls, rs) = match (l, r) {
        (
            Relation::Sort { source: ls, offset: lo, limit: ll, keys: lk },
            Relation::Sort { source: rs, offset: ro, limit: rl, keys: rk },
        ) if lk == rk && ll == rl && lo == ro => {
            // Keys over every column leave no tie the arrival order could settle.
            let width = ls.output_types(schemas).len();
            let total = (0..width).all(|c| lk.iter().any(|k| k.column as usize == c));
            if !total && (erases_an_order(ls, schemas) || erases_an_order(rs, schemas)) {
                return false;
            }
            (ls, rs)
        }
        (
            Relation::Group { source: ls, keys: lk, function: lf },
            Relation::Group { source: rs, keys: rk, function: rf },
        ) if lk == rk && lf == rf => {
            if erases_an_order(ls, schemas) || erases_an_order(rs, schemas) {
                return false;
            }
            (ls, rs)
        }
        _ => return false,
    };
    let (lt, rt) = (l.output_types(schemas), r.output_types(schemas));
    if lt != rt || lt.iter().any(Option::is_none) {
        return false;
    }
    ls == rs
        || (depth < MAX_DEPTH
            && matches!(
                prove_at(&Input { schemas: schemas.to_vec(), queries: [(**ls).clone(), (**rs).clone()] }, depth + 1),
                Verdict::Eq { .. }
            ))
}

/// Whether `source` holds an `ORDER BY` translation erases (one with keys that keeps every row).
/// Postgres hands a slice or a `DISTINCT ON` its rows in that order, so where their own keys tie,
/// it decides which rows they keep, and two sources equal as bags but ordered two ways give two
/// answers. The frontend refuses such a node (`lower::refuse_slice_over_dropped_order` and
/// `refuse_distinct_on_over_dropped_order`); the same refusal is kept here so that the claim this
/// module makes does not rest on it. Conservative: an `ORDER BY` anywhere below counts.
fn erases_an_order(source: &Relation, schemas: &[Schema]) -> bool {
    let mut found = false;
    each_node(&mut source.clone(), 0, schemas, &mut |r, base| {
        if let Relation::Sort { source, offset, limit, keys } = r {
            found |= !keys.is_empty()
                && card::slice(source, offset.as_ref(), limit.as_ref(), base, schemas) == Slice::Identity
                && !card::shape(source, base, schemas).max_one();
        }
        found
    });
    found
}

/// Visits every relation node of `r` in pre-order, subqueries inside its expressions included, with
/// the level each sits at; `f` returning `true` stops the descent below that node. A subquery's
/// level is its enclosing expression's level plus the width of the row that expression reads,
/// exactly as translation numbers it.
fn each_node(r: &mut Relation, base: usize, schemas: &[Schema], f: &mut dyn FnMut(&mut Relation, usize) -> bool) {
    if f(r, base) {
        return;
    }
    let width = |s: &Relation| s.output_types(schemas).len();
    match r {
        Relation::Scan(_) | Relation::Values { .. } => {}
        Relation::Distinct(source) => each_node(source, base, schemas, f),
        Relation::Filter { source, condition } => {
            let inner = base + width(source);
            each_node(source, base, schemas, f);
            each_node_in(condition, inner, schemas, f);
        }
        Relation::Project { source, target } => {
            let inner = base + width(source);
            each_node(source, base, schemas, f);
            target.iter_mut().for_each(|e| each_node_in(e, inner, schemas, f));
        }
        Relation::Join { left, right, condition, .. } => {
            let inner = base + width(left) + width(right);
            each_node(left, base, schemas, f);
            each_node(right, base, schemas, f);
            each_node_in(condition, inner, schemas, f);
        }
        Relation::Group { source, keys, function } => {
            let inner = base + width(source);
            each_node(source, base, schemas, f);
            keys.iter_mut().chain(function.iter_mut().flat_map(|c| c.operand.iter_mut())).for_each(|e| each_node_in(e, inner, schemas, f));
        }
        Relation::Sort { source, offset, limit, .. } => {
            let inner = base + width(source);
            each_node(source, base, schemas, f);
            offset.iter_mut().chain(limit.iter_mut()).for_each(|e| each_node_in(e, inner, schemas, f));
        }
        Relation::Union(sides) | Relation::Except(sides) | Relation::Intersect(sides) => {
            sides.iter_mut().for_each(|s| each_node(s, base, schemas, f));
        }
    }
}

fn each_node_in(e: &mut Expr, base: usize, schemas: &[Schema], f: &mut dyn FnMut(&mut Relation, usize) -> bool) {
    match e {
        Expr::Column { .. } | Expr::Literal { .. } => {}
        Expr::Cast { operand, .. } => each_node_in(operand, base, schemas, f),
        Expr::Call { operand, .. } => operand.iter_mut().for_each(|o| each_node_in(o, base, schemas, f)),
        Expr::Subquery { operand, query, .. } => {
            operand.iter_mut().for_each(|o| each_node_in(o, base, schemas, f));
            each_node(query, base, schemas, f);
        }
    }
}

/// Visits every expression node of `r`, those inside its subqueries included. Sort keys are
/// positions in the sort's source, not levels, and are not expressions.
fn each_expr(r: &mut Relation, f: &mut dyn FnMut(&mut Expr)) {
    match r {
        Relation::Scan(_) => {}
        Relation::Values { content, .. } => content.iter_mut().flatten().for_each(|e| each_subexpr(e, f)),
        Relation::Distinct(source) => each_expr(source, f),
        Relation::Filter { source, condition } => {
            each_expr(source, f);
            each_subexpr(condition, f);
        }
        Relation::Project { source, target } => {
            each_expr(source, f);
            target.iter_mut().for_each(|e| each_subexpr(e, f));
        }
        Relation::Join { left, right, condition, .. } => {
            each_expr(left, f);
            each_expr(right, f);
            each_subexpr(condition, f);
        }
        Relation::Group { source, keys, function } => {
            each_expr(source, f);
            keys.iter_mut().chain(function.iter_mut().flat_map(|c| c.operand.iter_mut())).for_each(|e| each_subexpr(e, f));
        }
        Relation::Sort { source, offset, limit, .. } => {
            each_expr(source, f);
            offset.iter_mut().chain(limit.iter_mut()).for_each(|e| each_subexpr(e, f));
        }
        Relation::Union(sides) | Relation::Except(sides) | Relation::Intersect(sides) => {
            sides.iter_mut().for_each(|s| each_expr(s, f));
        }
    }
}

fn each_subexpr(e: &mut Expr, f: &mut dyn FnMut(&mut Expr)) {
    f(e);
    match e {
        Expr::Column { .. } | Expr::Literal { .. } => {}
        Expr::Cast { operand, .. } => each_subexpr(operand, f),
        Expr::Call { operand, .. } => operand.iter_mut().for_each(|o| each_subexpr(o, f)),
        Expr::Subquery { operand, query, .. } => {
            operand.iter_mut().for_each(|o| each_subexpr(o, f));
            each_expr(query, f);
        }
    }
}
