// Unless explicitly stated otherwise all files in this repository are licensed under the
// Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/).
// Copyright 2026-Present Datadog, Inc.

//! Postgres's `MAX` and `MIN` return one input, and compare with the type's order, under which two
//! values that `=` calls equal are neither above the other. On a type whose `=` is not identity --
//! `numeric` (`2.0 = 2.00`, REAL), `interval` (`'1 day' = '24 hours'`), the opaque VARBINARY
//! (`double precision`, `numrange[]`) -- a group can hold two such values at the top, and Postgres
//! returns one of them. Binding the extremum to an input by identity gave the group one extremum per
//! such value, so each pair below was proved although Postgres separates it: over
//! `t = {(1, 1, 2.0), (2, 1, 2.00)}` the extremum's side has one row and the anti-join's two, and a
//! cast to text tells them apart. Over these types the extremum is now refused; over a type whose
//! `=` is identity it is proved as before.

mod support;

use serde_json::{json, Value};
use sqleq_solver::prove::Verdict;
use support::*;

/// `t(id INTEGER, g INTEGER, x <ty>)`, every column NOT NULL, with `x` vouched for as a column whose
/// `=` is identity when `vouched` (a `bytea` column, say).
fn t_with(ty: &str, vouched: bool) -> Vec<Value> {
    let mut t = table("t", &["INTEGER", "INTEGER", ty], &[false, false, false], &[]);
    if vouched {
        t["opaque_identity"] = json!([false, false, true]);
    }
    vec![t]
}

/// The comparison an input beyond the extremum satisfies: `>` for `MAX`, `<` for `MIN`.
fn beyond(op: &str) -> &'static str {
    if op == "MAX" {
        ">"
    } else {
        "<"
    }
}

/// `t LEFT JOIN t AS u ON <on> WHERE u.id IS NULL`, the rows of `t` that `on` matches with none.
fn unmatched(on: Value) -> Value {
    let join = json!({ "join": { "left": scan(0), "right": scan(0), "kind": "LEFT", "condition": on } });
    filter(join, pred("IS NULL", vec![col(3, "INTEGER")]))
}

/// `SELECT DISTINCT CAST(m AS TEXT) FROM (SELECT <op>(x) AS m FROM t) AS s WHERE m IS NOT NULL`.
fn scalar_extremum(op: &str, ty: &str) -> Value {
    let m = group(project(scan(0), vec![col(2, ty)]), vec![], vec![agg(op, ty, vec![col(0, ty)])]);
    let m = filter(m, pred("IS NOT NULL", vec![col(0, ty)]));
    distinct(project(m, vec![cast("VARCHAR", col(0, ty))]), &["VARCHAR"])
}

/// `SELECT DISTINCT CAST(t.x AS TEXT) FROM t LEFT JOIN t AS u ON u.x > t.x WHERE u.id IS NULL`, with
/// `<` for `MIN`: every input with nothing beyond it.
fn scalar_antijoin(op: &str, ty: &str) -> Value {
    let rows = unmatched(pred(beyond(op), vec![col(5, ty), col(2, ty)]));
    distinct(project(rows, vec![cast("VARCHAR", col(2, ty))]), &["VARCHAR"])
}

/// `SELECT DISTINCT g, CAST(m AS TEXT) FROM (SELECT g, <op>(x) AS m FROM t GROUP BY g) AS s`.
fn grouped_extremum(op: &str, ty: &str) -> Value {
    let g = || col(0, "INTEGER");
    let m = group(project(scan(0), vec![col(1, "INTEGER"), col(2, ty)]), vec![g()], vec![agg(op, ty, vec![col(1, ty)])]);
    distinct(project(m, vec![g(), cast("VARCHAR", col(1, ty))]), &["INTEGER", "VARCHAR"])
}

/// `SELECT DISTINCT t.g, CAST(t.x AS TEXT) FROM t LEFT JOIN t AS u ON u.g = t.g AND u.x > t.x WHERE
/// u.id IS NULL`, with `<` for `MIN`.
fn grouped_antijoin(op: &str, ty: &str) -> Value {
    let same_g = pred("=", vec![col(4, "INTEGER"), col(1, "INTEGER")]);
    let rows = unmatched(pred("AND", vec![same_g, pred(beyond(op), vec![col(5, ty), col(2, ty)])]));
    distinct(project(rows, vec![col(1, "INTEGER"), cast("VARCHAR", col(2, ty))]), &["INTEGER", "VARCHAR"])
}

/// The extremum against the anti-join, for `MAX` and `MIN`, without and with `GROUP BY`.
fn pairs(ty: &str, vouched: bool) -> Vec<(String, Verdict)> {
    let mut out = Vec::new();
    for op in ["MAX", "MIN"] {
        let scalar = pair(t_with(ty, vouched), scalar_extremum(op, ty), scalar_antijoin(op, ty));
        out.push((op.to_string(), scalar));
        let grouped = pair(t_with(ty, vouched), grouped_extremum(op, ty), grouped_antijoin(op, ty));
        out.push((format!("{op} per group"), grouped));
    }
    out
}

#[test]
fn an_extremum_over_a_type_whose_equality_is_not_identity_is_refused() {
    let mut wrong = Vec::new();
    for ty in ["VARBINARY", "REAL", "INTERVAL"] {
        for (op, v) in pairs(ty, false) {
            if v.to_string() != format!("NOTRANS extremum-not-identity:{ty}") {
                wrong.push(format!("{op} over {ty}: {v}"));
            }
        }
    }
    assert!(wrong.is_empty(), "not refused:\n{}", wrong.join("\n"));
}

#[test]
fn an_extremum_over_a_type_whose_equality_is_identity_is_still_proved() {
    // The control: the same pairs, which are equivalent when `=` is identity, a VARBINARY column
    // the schema vouches for (`bytea`) included.
    let mut wrong = Vec::new();
    for (ty, vouched) in [("INTEGER", false), ("VARCHAR", false), ("DATE", false), ("TIMESTAMP", false), ("VARBINARY", true)] {
        for (op, v) in pairs(ty, vouched) {
            if v != (Verdict::Eq { literal: false }) {
                wrong.push(format!("{op} over {ty}: {v}"));
            }
        }
    }
    assert!(wrong.is_empty(), "not proved:\n{}", wrong.join("\n"));
}

#[test]
fn an_aggregate_that_is_not_an_extremum_is_not_refused() {
    // `COUNT` and `SUM` do not pick an input, so they are translated over a REAL column as before.
    let schemas = || t_with("REAL", false);
    let x = || project(scan(0), vec![col(2, "REAL")]);
    let x_where_true = || project(filter(scan(0), lit("TRUE", "BOOLEAN")), vec![col(2, "REAL")]);
    for (op, ty) in [("COUNT", "INTEGER"), ("SUM", "REAL")] {
        let of = |source: Value| group(source, vec![], vec![agg(op, ty, vec![col(0, "REAL")])]);
        let v = pair(schemas(), of(x()), of(x_where_true()));
        assert!(!matches!(v, Verdict::Refused(_)), "{op}: {v:?}");
    }
}
