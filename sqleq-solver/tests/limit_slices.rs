// Unless explicitly stated otherwise all files in this repository are licensed under the
// Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/).
// Copyright 2026-Present Datadog, Inc.

//! Row slices: a `LIMIT` that keeps every row of a source with at most one row (`card`), and the
//! same slice of equal sources on both sides (`opaque`). Each "not proved" pair below differs in
//! Postgres, or is one only the frontend's refusals keep from reaching the prover.

mod support;

use serde_json::{json, Value};
use sqleq_solver::ir::TranslateError;
use sqleq_solver::prove::Verdict;
use support::*;

/// `t(k, a, b, c)`, keys `(k)` and `(b, c)`, `a` nullable; `u(id, t_k)`, key `(id)`; `n(x)`, a
/// unique key over a nullable column; `v(a, b)` and `w(a, b)`, no keys; `r(k REAL)`, key `(k)`.
fn schemas() -> Vec<Value> {
    let int4 = ["INTEGER"; 4];
    vec![
        table("t", &int4, &[false, true, false, false], &[&[0], &[2, 3]]),
        table("u", &int4[..2], &[false, true], &[&[0]]),
        table("n", &int4[..1], &[true], &[&[0]]),
        table("v", &int4[..2], &[true, true], &[]),
        table("w", &int4[..2], &[true, true], &[]),
        table("r", &["REAL"], &[false], &[&[0]]),
    ]
}

fn int(i: u32) -> Value {
    col(i, "INTEGER")
}

fn param(n: u32) -> Value {
    call(&format!("QP{n}"), "INTEGER", vec![lit("0", "INTEGER")])
}

fn eq(a: Value, b: Value) -> Value {
    pred("=", vec![a, b])
}

fn and(parts: Vec<Value>) -> Value {
    pred("AND", parts)
}

/// `ORDER BY keys LIMIT limit OFFSET offset` over `source`; `keys` are `(position, descending)`.
fn page(source: Value, keys: &[(u32, bool)], limit: Option<Value>, offset: Option<Value>) -> Value {
    let collation: Vec<Value> = keys
        .iter()
        .map(|(c, desc)| json!([c, "INTEGER", if *desc { "DESCENDING NULLS FIRST" } else { "ASCENDING NULLS LAST" }]))
        .collect();
    json!({ "sort": { "source": source, "collation": collation, "limit": limit, "offset": offset } })
}

fn limit(source: Value, n: Value) -> Value {
    page(source, &[], Some(n), None)
}

fn one() -> Value {
    lit("1", "INTEGER")
}

/// `SELECT a FROM t WHERE cond`.
fn a_where(cond: Value) -> Value {
    project(filter(scan(0), cond), vec![int(1)])
}

fn in_(operand: Vec<Value>, query: Value) -> Value {
    json!({ "operator": "IN", "type": "BOOLEAN", "operand": operand, "query": query })
}

fn verdict(a: Value, b: Value) -> Verdict {
    pair(schemas(), a, b)
}

#[test]
fn limit_one_over_a_key_held_to_a_parameter_keeps_every_row() {
    let q = || a_where(eq(int(0), param(1)));
    assert_eq!(verdict(q(), limit(q(), one())), Verdict::Eq { literal: false });
    // Any literal count of one or more, and an `OFFSET 0`.
    assert!(proved(&verdict(q(), page(q(), &[(0, false)], Some(lit("10", "INTEGER")), Some(lit("0", "INTEGER"))))));
    // Both columns of the composite key.
    let bc = || a_where(and(vec![eq(int(2), param(1)), eq(param(2), int(3))]));
    assert!(proved(&verdict(bc(), limit(bc(), one()))));
}

#[test]
fn a_key_held_through_a_join_or_a_group_keeps_every_row() {
    // `SELECT t.a FROM u JOIN t ON t.k = u.t_k WHERE u.id = $1`: `t`'s columns follow `u`'s.
    let joined = || {
        let join = json!({ "join": { "kind": "INNER", "left": scan(1), "right": scan(0), "condition": eq(int(2), int(1)) } });
        project(filter(join, eq(int(0), param(1))), vec![int(3)])
    };
    assert!(proved(&verdict(joined(), limit(joined(), one()))));
    // `SELECT a, count(*) FROM t WHERE a = $1 GROUP BY a`: one group at most.
    let grouped = || group(project(filter(scan(0), eq(int(1), param(1))), vec![int(1)]), vec![int(0)], vec![agg("COUNT", "INTEGER", vec![])]);
    assert!(proved(&verdict(grouped(), limit(grouped(), one()))));
}

#[test]
fn limit_zero_and_offset_zero() {
    let all = || project(scan(0), vec![int(1)]);
    assert!(proved(&verdict(limit(all(), lit("0", "INTEGER")), a_where(lit("FALSE", "BOOLEAN")))));
    assert!(proved(&verdict(page(all(), &[(0, false)], None, Some(lit("0", "INTEGER"))), all())));
}

#[test]
fn a_limit_under_exists_keeps_its_answer() {
    // `EXISTS (SELECT 1 FROM u WHERE u.t_k = $1 [LIMIT 1])`: `u`'s columns follow `t`'s.
    let sub = || project(filter(scan(1), eq(int(5), param(1))), vec![one()]);
    let ks = |cond: Value| project(filter(scan(0), cond), vec![int(0)]);
    assert!(proved(&verdict(ks(exists(sub())), ks(exists(limit(sub(), one()))))));
    // `OFFSET 1` can make it empty.
    let skipped = page(sub(), &[], Some(one()), Some(one()));
    assert!(!proved(&verdict(ks(exists(sub())), ks(exists(skipped)))));
}

#[test]
fn a_limit_over_rows_no_key_holds_is_not_the_identity() {
    let cases = [
        (0, eq(int(1), param(1))),                                  // `a` is no key
        (0, eq(int(2), param(1))),                                  // half of `(b, c)`
        (0, pred("<=", vec![int(0), param(1)])),                    // an order comparison
        (0, pred("OR", vec![eq(int(0), param(1)), eq(int(0), param(2))])),
        (2, eq(int(0), param(1))),                                  // a key over a nullable column
        (5, eq(col(0, "REAL"), call("QP1", "REAL", vec![lit("0", "INTEGER")]))), // `=` on REAL
    ];
    for (table, cond) in cases {
        let q = || project(filter(scan(table), cond.clone()), vec![int(0)]);
        assert!(!proved(&verdict(q(), limit(q(), one()))), "{cond}");
    }
    // A parameter count may be 0; an offset may skip the one row.
    let q = || a_where(eq(int(0), param(1)));
    assert!(!proved(&verdict(q(), limit(q(), param(2)))));
    assert!(!proved(&verdict(q(), page(q(), &[], Some(one()), Some(one())))));
}

/// `SELECT k FROM t WHERE <a = $1 AND b = $2, in either order> ORDER BY c LIMIT $3`, lowered with
/// the order key in the slice's source (`sort_sandwich`) and trimmed above it.
fn page_of_t(swapped: bool, keys: &[(u32, bool)], limit: Value) -> Value {
    let (x, y) = (eq(int(1), param(1)), eq(int(2), param(2)));
    let cond = if swapped { and(vec![y, x]) } else { and(vec![x, y]) };
    project(page(project(filter(scan(0), cond), vec![int(0), int(3)]), keys, Some(limit), None), vec![int(0)])
}

#[test]
fn the_same_slice_of_equal_sources_is_one_relation() {
    let by_c = [(1, false)];
    assert_eq!(verdict(page_of_t(false, &by_c, param(3)), page_of_t(true, &by_c, param(3))), Verdict::Eq { literal: false });
    // Nested in an `IN` subquery, whose columns follow the outer `t`'s.
    let sub = |swapped: bool| {
        let (x, y) = (eq(int(5), param(1)), eq(int(6), param(2)));
        let cond = if swapped { and(vec![y, x]) } else { and(vec![x, y]) };
        project(page(project(filter(scan(0), cond), vec![int(4), int(7)]), &by_c, Some(param(3)), None), vec![int(4)])
    };
    let outer = |swapped| project(filter(scan(0), in_(vec![int(0)], sub(swapped))), vec![int(1)]);
    assert!(proved(&verdict(outer(false), outer(true))));
}

#[test]
fn a_different_slice_is_not_the_same_relation() {
    let p3 = || param(3);
    for (keys, lim) in [(vec![(1, true)], p3()), (vec![(0, false)], p3()), (vec![(1, false)], param(4))] {
        let v = verdict(page_of_t(false, &[(1, false)], p3()), page_of_t(true, &keys, lim));
        assert_eq!(v, Verdict::Refused(TranslateError::UnsupportedSort));
    }
    // Two slices on one side, one on the other.
    let two = json!({ "union": [page_of_t(false, &[(1, false)], p3()), page_of_t(false, &[(1, false)], p3())] });
    let one_and_scan = json!({ "union": [page_of_t(true, &[(1, false)], p3()), project(scan(0), vec![int(0)])] });
    assert_eq!(verdict(two, one_and_scan), Verdict::Refused(TranslateError::UnsupportedSort));
}

#[test]
fn a_slice_of_a_correlated_source_is_left_refused() {
    // `SELECT k FROM t WHERE c = $2 AND a = (SELECT t_k FROM u WHERE u.t_k = t.b ORDER BY id LIMIT $1)`.
    let sub = || {
        let src = filter(scan(1), eq(int(5), int(2)));
        scalar(project(page(src, &[(0, false)], Some(param(1)), None), vec![int(5)]), "INTEGER")
    };
    let (x, y) = (|| eq(int(3), param(2)), || eq(int(1), sub()));
    let a = project(filter(scan(0), and(vec![x(), y()])), vec![int(0)]);
    let b = project(filter(scan(0), and(vec![y(), x()])), vec![int(0)]);
    assert_eq!(verdict(a, b), Verdict::Refused(TranslateError::UnsupportedSort));
}

#[test]
fn equal_bags_under_a_slice_are_not_enough_when_the_slice_reads_more() {
    // `SELECT a FROM v ORDER BY b LIMIT 1` against the same over `w`: as `SELECT a`, both may be
    // the bag {1, 2}, but over v = {(1,1), (2,2)} and w = {(1,2), (2,1)} one yields 1, the other 2.
    // The order key is in the slice's source, so the sources differ.
    let first_a = |table| project(page(project(scan(table), vec![int(0), int(1)]), &[(1, false)], Some(one()), None), vec![int(0)]);
    assert!(!proved(&verdict(first_a(3), first_a(4))));
    // `SELECT k FROM (SELECT k FROM t ORDER BY k [DESC]) s LIMIT 1`: min(k) against max(k). The
    // frontend refuses a slice over a subquery order it drops; this module refuses it as well.
    let first_of = |desc| limit(page(project(scan(0), vec![int(0)]), &[(0, desc)], None, None), one());
    assert!(!proved(&verdict(first_of(false), first_of(true))));
    // Ordered by every column it returns, ties are equal rows and the order beneath does not matter.
    let ordered = |desc| page(page(project(scan(0), vec![int(0)]), &[(0, desc)], None, None), &[(0, false)], Some(one()), None);
    assert!(proved(&verdict(ordered(false), ordered(true))));
}

#[test]
fn the_same_distinct_on_of_equal_sources_is_one_relation() {
    // `SELECT DISTINCT ON (a) a, k FROM t WHERE <b = $1 AND c = $2, in either order> ORDER BY a, k`.
    let distinct_on = |swapped: bool| {
        let (x, y) = (eq(int(2), param(1)), eq(int(3), param(2)));
        let cond = if swapped { and(vec![y, x]) } else { and(vec![x, y]) };
        let pick = json!({ "operator": "DISTINCT_ON#0<0:ASCENDING NULLS LAST;1:ASCENDING NULLS LAST;>",
                           "type": "INTEGER", "distinct": false, "ignoreNulls": false, "operand": [int(1), int(0)] });
        group(filter(scan(0), cond), vec![int(1)], vec![pick])
    };
    assert!(proved(&verdict(distinct_on(false), distinct_on(true))));
}

#[test]
fn a_from_less_select_reads_its_one_row() {
    // `SELECT EXISTS (SELECT 1 FROM t WHERE <a = $1 AND b = $2, in either order> [LIMIT 1])`.
    let sub = |swapped: bool| {
        let (x, y) = (eq(int(1), param(1)), eq(int(2), param(2)));
        project(filter(scan(0), if swapped { and(vec![y, x]) } else { and(vec![x, y]) }), vec![one()])
    };
    let select = |e: Value| project(json!("singleton"), vec![e]);
    assert!(proved(&verdict(select(exists(sub(false))), select(exists(sub(true))))));
    assert!(proved(&verdict(select(exists(sub(false))), select(exists(limit(sub(false), one()))))));
}

#[test]
fn a_values_cell_may_compute_its_value() {
    let values = |cell: Value| json!({ "values": { "schema": ["INTEGER"], "content": [[cell]] } });
    // `SELECT x FROM (VALUES ($1)) v(x)`, filtered on TRUE or not.
    let plain = project(values(param(1)), vec![int(0)]);
    let filtered = project(filter(values(param(1)), lit("TRUE", "BOOLEAN")), vec![int(0)]);
    assert!(proved(&verdict(plain, filtered)));
    // A cell of another type than its column is refused, not coerced.
    let text = call("QP1", "VARCHAR", vec![lit("0", "INTEGER")]);
    assert_eq!(verdict(project(values(text), vec![int(0)]), project(scan(1), vec![int(0)])), Verdict::Refused(TranslateError::ValuesNonLiteral));
}

#[test]
fn count_distinct_counts_the_distinct_non_null_values() {
    let count_distinct = |c: u32| {
        let call = json!({ "operator": "COUNT", "type": "INTEGER", "distinct": true, "ignoreNulls": true, "operand": [int(0)] });
        group(project(filter(scan(0), eq(int(0), param(1))), vec![int(c)]), vec![], vec![call])
    };
    // `SELECT count(*) FROM (SELECT DISTINCT <c> FROM t WHERE k = $1) d`.
    let count_of_distinct = |c: u32| {
        let distinct_c = distinct(project(filter(scan(0), eq(int(0), param(1))), vec![int(c)]), &["INTEGER"]);
        group(distinct_c, vec![], vec![agg("COUNT", "INTEGER", vec![])])
    };
    // `b` is NOT NULL: the two agree.
    assert!(proved(&verdict(count_distinct(2), count_of_distinct(2))));
    // `a` is nullable: `count(DISTINCT a)` skips the NULL that `SELECT DISTINCT a` keeps.
    assert!(!proved(&verdict(count_distinct(1), count_of_distinct(1))));
    // `sum(DISTINCT ...)` stays refused.
    let sum = json!({ "operator": "SUM", "type": "INTEGER", "distinct": true, "ignoreNulls": true, "operand": [int(0)] });
    let summed = group(project(scan(0), vec![int(2)]), vec![], vec![sum]);
    let plain = group(project(scan(0), vec![int(2)]), vec![], vec![agg("SUM", "INTEGER", vec![int(0)])]);
    assert_eq!(verdict(summed, plain), Verdict::Refused(TranslateError::DistinctAggregateUnsupported));
}
