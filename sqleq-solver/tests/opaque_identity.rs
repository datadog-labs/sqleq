// Unless explicitly stated otherwise all files in this repository are licensed under the
// Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/).
// Copyright 2026-Present Datadog, Inc.

//! The opaque VARBINARY stands for `double precision`, `jsonb` and `numeric[]`, whose `=` is not
//! identity, so `=` between two VARBINARY values is read through a key and deduplicating them is
//! refused. It also stands for `bytea` and `uuid`, whose `=` is identity, and a schema says which
//! columns those are (`opaque_identity`). On a column it vouches for, `=` licenses substituting one
//! side for the other and a deduplication is translated, as on an INTEGER; anything that is not a
//! bare reference to such a column is what its IR type says.

mod support;

use serde_json::{json, Value};
use sqleq_solver::prove::Verdict;
use support::*;

/// `name(id INTEGER, h VARBINARY)`, with `h` vouched for when `vouched`.
fn table_h(name: &str, vouched: bool) -> Value {
    let mut t = table(name, &["INTEGER", "VARBINARY"], &[true, true], &[&[0]]);
    if vouched {
        t["opaque_identity"] = json!([false, true]);
    }
    t
}

/// `SELECT f(t.h) FROM t JOIN u ON t.h = u.h` (`side` 0) or `SELECT f(u.h) ...` (`side` 1).
fn join_on_h(side: u32, f: impl Fn(Value) -> Value) -> Value {
    let on = pred("=", vec![col(1, "VARBINARY"), col(3, "VARBINARY")]);
    let join = json!({ "join": { "left": scan(0), "right": scan(1), "kind": "INNER", "condition": on } });
    project(join, vec![f(col(1 + 2 * side, "VARBINARY"))])
}

fn bare(x: Value) -> Value {
    x
}

fn text(x: Value) -> Value {
    cast("VARCHAR", x)
}

#[test]
fn an_equality_on_a_vouched_column_lets_either_side_be_projected() {
    // `bytea`: t.h = u.h means t.h and u.h are the same bytes, so either may be projected, or cast
    // to text.
    for f in [bare, text] {
        let v = pair(vec![table_h("t", true), table_h("u", true)], join_on_h(0, f), join_on_h(1, f));
        assert_eq!(v, Verdict::Eq { literal: false }, "{v:?}");
    }
}

#[test]
fn an_equality_on_an_unvouched_column_is_still_read_through_a_key() {
    // `double precision` or `numeric[]`: t.h = u.h holds on 0 and -0, or on {2.0} and {2.00}. Both
    // tables must vouch: one alone says nothing about the other's column.
    for (tv, uv) in [(false, false), (true, false), (false, true)] {
        let v = pair(vec![table_h("t", tv), table_h("u", uv)], join_on_h(0, text), join_on_h(1, text));
        assert!(!proved(&v), "t vouched {tv}, u vouched {uv}: {v:?}");
    }
}

/// `SELECT DISTINCT CAST(x AS TEXT) FROM (SELECT DISTINCT x FROM (<source>))` against the same
/// without the inner `DISTINCT`, `source` yielding `x` as its one column.
fn distinct_pair(schemas: Vec<Value>, source: Value) -> Verdict {
    let outer = |s: Value| distinct(project(s, vec![text(col(0, "VARBINARY"))]), &["VARCHAR"]);
    pair(schemas, outer(distinct(source.clone(), &["VARBINARY"])), outer(source))
}

#[test]
fn a_deduplication_over_a_vouched_column_is_translated() {
    let h = project(scan(0), vec![col(1, "VARBINARY")]);
    assert_eq!(distinct_pair(vec![table_h("t", true)], h.clone()), Verdict::Eq { literal: false });
    let v = distinct_pair(vec![table_h("t", false)], h);
    assert_eq!(v.to_string(), "NOTRANS dedup-not-identity:VARBINARY");
}

#[test]
fn only_a_bare_reference_to_a_vouched_column_is_vouched_for() {
    let refused = |v: Verdict| v.to_string() == "NOTRANS dedup-not-identity:VARBINARY";
    let t = || vec![table_h("t", true)];
    // Through a function, even one that returns its argument: what a call returns is its IR type.
    let coalesce = project(scan(0), vec![call("COALESCE", "VARBINARY", vec![col(1, "VARBINARY")])]);
    assert!(refused(distinct_pair(t(), coalesce)));
    // A union keeps the vouching only where both branches have it.
    let schemas = vec![table("t", &["INTEGER", "VARBINARY", "VARBINARY"], &[true, true, true], &[])];
    let mut schemas_one = schemas.clone();
    schemas_one[0]["opaque_identity"] = json!([false, true, false]);
    let union = |a: u32, b: u32| {
        json!({ "union": [project(scan(0), vec![col(a, "VARBINARY")]), project(scan(0), vec![col(b, "VARBINARY")])] })
    };
    assert!(refused(distinct_pair(schemas_one.clone(), union(1, 2))));
    assert!(!refused(distinct_pair(schemas_one, union(1, 1))));
    // A flag on a column that is not VARBINARY changes nothing: a REAL's `=` is never identity.
    let mut real = table("t", &["INTEGER", "REAL"], &[true, true], &[]);
    real["opaque_identity"] = json!([true, true]);
    let x = project(scan(0), vec![col(1, "REAL")]);
    let v = pair(vec![real], distinct(x.clone(), &["REAL"]), x);
    assert_eq!(v.to_string(), "NOTRANS dedup-not-identity:REAL");
}
