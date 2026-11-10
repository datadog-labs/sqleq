// Unless explicitly stated otherwise all files in this repository are licensed under the
// Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/).
// Copyright 2026-Present Datadog, Inc.

//! A scalar subquery is NULL when it returns no row *or* its one row holds NULL. Each pair below is
//! not equivalent in Postgres, and each was proved while the subquery's null flag was "no row".

mod support;

use serde_json::Value;
use support::*;

/// `t(id)` and `s(k, y)`, `k` unique.
fn schemas() -> Vec<Value> {
    vec![table("t", &["INTEGER"], &[true], &[]), table("s", &["INTEGER", "INTEGER"], &[true, true], &[&[0]])]
}

/// `SELECT y FROM s WHERE k = 1`, inside a query over `t(id)`: `s`'s columns follow `t`'s.
fn y_where_k_is_1() -> Value {
    project(filter(scan(1), pred("=", vec![col(1, "INTEGER"), lit("1", "INTEGER")])), vec![col(2, "INTEGER")])
}

fn ids_where(cond: Value) -> Value {
    project(filter(scan(0), cond), vec![col(0, "INTEGER")])
}

#[test]
fn a_one_row_subquery_holding_null_is_null() {
    // t = {(1)}, s = {(1, NULL)}: the subquery returns one row, whose value is NULL.
    let sub = || scalar(y_where_k_is_1(), "INTEGER");
    let is_null = ids_where(pred("IS NULL", vec![sub()]));
    let not_exists = ids_where(pred("NOT", vec![exists(y_where_k_is_1())]));
    assert!(!proved(&pair(schemas(), is_null, not_exists)));

    let is_not_null = ids_where(pred("IS NOT NULL", vec![sub()]));
    let does_exist = ids_where(exists(y_where_k_is_1()));
    assert!(!proved(&pair(schemas(), is_not_null, does_exist)));

    let projected = project(scan(0), vec![pred("IS NULL", vec![sub()])]);
    let projected_not_exists = project(scan(0), vec![pred("NOT", vec![exists(y_where_k_is_1())])]);
    assert!(!proved(&pair(schemas(), projected, projected_not_exists)));
}

#[test]
fn null_is_no_row_holding_a_non_null_value() {
    // The reading the fix gives, stated another way, still proves.
    let is_null = ids_where(pred("IS NULL", vec![scalar(y_where_k_is_1(), "INTEGER")]));
    let y_not_null = project(
        filter(
            scan(1),
            pred("AND", vec![pred("=", vec![col(1, "INTEGER"), lit("1", "INTEGER")]), pred("IS NOT NULL", vec![col(2, "INTEGER")])]),
        ),
        vec![col(2, "INTEGER")],
    );
    let no_such_row = ids_where(pred("NOT", vec![exists(y_not_null)]));
    assert!(proved(&pair(schemas(), is_null, no_such_row)));
}

/// `SELECT sum(y) FROM s` (or `count(*)`) inside a query over a table of `outer` columns.
fn scalar_agg(op: &str, outer: u32) -> Value {
    let operand = if op == "COUNT" { vec![] } else { vec![col(outer, "INTEGER")] };
    let q = group(project(scan(1), vec![col(outer + 1, "INTEGER")]), vec![], vec![agg(op, "INTEGER", operand)]);
    scalar(q, "INTEGER")
}

#[test]
fn a_scalar_aggregate_over_no_rows_is_null() {
    // t = {(1)}, s = {}: sum over no rows is NULL.
    let sum = || scalar_agg("SUM", 1);
    let all = || project(scan(0), vec![col(0, "INTEGER")]);
    assert!(!proved(&pair(schemas(), ids_where(pred("IS NOT NULL", vec![sum()])), all())));
    assert!(!proved(&pair(schemas(), ids_where(pred("IS NULL", vec![sum()])), ids_where(lit("FALSE", "BOOLEAN")))));
    let projected = project(scan(0), vec![pred("IS NULL", vec![sum()])]);
    assert!(!proved(&pair(schemas(), projected, project(scan(0), vec![lit("FALSE", "BOOLEAN")]))));
    let count = scalar_agg("COUNT", 1);
    assert!(!proved(&pair(
        schemas(),
        ids_where(pred("IS NOT NULL", vec![sum()])),
        ids_where(pred("IS NOT NULL", vec![count]))
    )));
}

#[test]
fn a_comparison_with_a_null_subquery_is_unknown() {
    // t = {(1, 5)}, s = {}: `5 = NULL` is UNKNOWN, so NOT of it rejects the row and IS NOT TRUE
    // keeps it.
    let schemas = vec![table("t", &["INTEGER", "INTEGER"], &[true, false], &[]), table("s", &["INTEGER", "INTEGER"], &[true, true], &[])];
    let eq = |lhs: Value| pred("=", vec![lhs, scalar_agg("SUM", 2)]);
    for lhs in [col(1, "INTEGER"), lit("5", "INTEGER")] {
        let negated = ids_where(pred("NOT", vec![eq(lhs.clone())]));
        let not_true = ids_where(pred("IS NOT TRUE", vec![eq(lhs.clone())]));
        assert!(!proved(&pair(schemas.clone(), negated, not_true)), "WHERE, against {lhs}");
        let negated = project(scan(0), vec![pred("NOT", vec![eq(lhs.clone())])]);
        let not_true = project(scan(0), vec![pred("IS NOT TRUE", vec![eq(lhs.clone())])]);
        assert!(!proved(&pair(schemas.clone(), negated, not_true)), "SELECT, against {lhs}");
    }
}
