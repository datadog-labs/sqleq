// Unless explicitly stated otherwise all files in this repository are licensed under the
// Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/).
// Copyright 2026-Present Datadog, Inc.

//! Postgres deduplicates with the type's `=`: `DISTINCT`, `GROUP BY`, `UNION`, `INTERSECT` and
//! `EXCEPT` keep one row per class of `=`-equal values. On a type whose `=` is not identity --
//! `double precision` (`0 = -0`, an opaque VARBINARY in the IR), `numeric` (`2.0 = 2.00`, REAL),
//! `interval` (`'1 day' = '24 hours'`) -- that is fewer rows than deduplicating by identity, and
//! which value of a class survives is up to Postgres. A deduplication by identity made
//! `f(DISTINCT x)` have the rows of `DISTINCT f(x)` for every `f`, a cast to text included, so each
//! pair below was proved although Postgres separates it: over `t = {(1, 0), (2, -0)}` (or `2.0` and
//! `2.00`, or `'1 day'` and `'24 hours'`) the deduplicating side has one row and the other two. Over
//! these types the deduplication is now refused; over a type whose `=` is identity it is proved as
//! before.

mod support;

use serde_json::{json, Value};
use sqleq_solver::prove::Verdict;
use support::*;

/// `t(id INTEGER, x <ty>)`.
fn t_with(ty: &str) -> Vec<Value> {
    vec![table("t", &["INTEGER", ty], &[true, true], &[])]
}

/// `SELECT x FROM t`.
fn xs(ty: &str) -> Value {
    project(scan(0), vec![col(1, ty)])
}

/// `SELECT x FROM t WHERE id IS NULL AND id IS NOT NULL`: no rows, written so that only a solver
/// that reasons about it knows.
fn no_xs(ty: &str) -> Value {
    let id = || col(0, "INTEGER");
    let never = pred("AND", vec![pred("IS NULL", vec![id()]), pred("IS NOT NULL", vec![id()])]);
    project(filter(scan(0), never), vec![col(1, ty)])
}

/// The five ways to deduplicate `SELECT x FROM t`, as the frontend lowers each: `DISTINCT` and
/// `GROUP BY x` are both a keys-only group, `UNION` a `distinct` over a `union`.
fn deduplications(ty: &str) -> Vec<(&'static str, Value)> {
    vec![
        ("DISTINCT", distinct(xs(ty), &[ty])),
        ("GROUP BY", group(xs(ty), vec![col(0, ty)], vec![])),
        ("UNION", json!({ "distinct": { "union": [xs(ty), xs(ty)] } })),
        ("INTERSECT", json!({ "intersect": [xs(ty), xs(ty)] })),
        ("EXCEPT", json!({ "except": [xs(ty), no_xs(ty)] })),
    ]
}

/// `SELECT DISTINCT CAST(x AS TEXT) FROM (<source>) AS s`, `source` yielding `x` as its one column.
fn distinct_text_of(source: Value, ty: &str) -> Value {
    distinct(project(source, vec![cast("VARCHAR", col(0, ty))]), &["VARCHAR"])
}

/// Query A of each pair: the cast over a deduplication. Query B: `SELECT DISTINCT CAST(x AS TEXT)
/// FROM t`.
fn pairs(ty: &str) -> Vec<(&'static str, Verdict)> {
    deduplications(ty)
        .into_iter()
        .map(|(op, dedup)| (op, pair(t_with(ty), distinct_text_of(dedup, ty), distinct_text_of(xs(ty), ty))))
        .collect()
}

#[test]
fn a_deduplication_over_a_type_whose_equality_is_not_identity_is_refused() {
    let mut wrong = Vec::new();
    for ty in ["VARBINARY", "REAL", "INTERVAL"] {
        for (op, v) in pairs(ty) {
            if v.to_string() != format!("NOTRANS dedup-not-identity:{ty}") {
                wrong.push(format!("{op} over {ty}: {v}"));
            }
        }
    }
    assert!(wrong.is_empty(), "not refused:\n{}", wrong.join("\n"));
}

#[test]
fn a_deduplication_over_a_type_whose_equality_is_identity_is_still_proved() {
    // The control: the same pairs, which are equivalent when `=` is identity.
    let mut wrong = Vec::new();
    for ty in ["INTEGER", "VARCHAR", "DATE", "TIMESTAMP"] {
        for (op, v) in pairs(ty) {
            if v != (Verdict::Eq { literal: false }) {
                wrong.push(format!("{op} over {ty}: {v}"));
            }
        }
    }
    assert!(wrong.is_empty(), "not proved:\n{}", wrong.join("\n"));
}

#[test]
fn a_set_operation_column_of_two_types_is_refused_as_unresolved() {
    // `INTERSECT` of an INTEGER column with a REAL one: Postgres compares them as numerics, and the
    // IR does not record the type it resolves the column to.
    let schemas = vec![table("t", &["INTEGER", "INTEGER", "REAL"], &[true, true, true], &[])];
    let mixed = json!({ "intersect": [project(scan(0), vec![col(1, "INTEGER")]), project(scan(0), vec![col(2, "REAL")])] });
    let v = pair(schemas, mixed.clone(), mixed.clone());
    // Identical sides are answered before anything is translated.
    assert_eq!(v, Verdict::Eq { literal: true });
    let v = pair(
        vec![table("t", &["INTEGER", "INTEGER", "REAL"], &[true, true, true], &[])],
        mixed,
        project(scan(0), vec![col(1, "INTEGER")]),
    );
    assert_eq!(v.to_string(), "NOTRANS dedup-not-identity:unresolved");
}

#[test]
fn what_does_not_deduplicate_is_not_refused() {
    // `UNION ALL` adds the two sides, and a scalar aggregate has no keys: neither keeps one row per
    // class, so neither is refused over a REAL column.
    let union_all = |a: Value, b: Value| json!({ "union": [a, b] });
    let v = pair(
        t_with("REAL"),
        project(union_all(xs("REAL"), no_xs("REAL")), vec![cast("VARCHAR", col(0, "REAL"))]),
        project(union_all(no_xs("REAL"), xs("REAL")), vec![cast("VARCHAR", col(0, "REAL"))]),
    );
    assert!(!matches!(v, Verdict::Refused(_)), "{v:?}");
    let count = |source: Value| group(source, vec![], vec![agg("COUNT", "INTEGER", vec![col(0, "REAL")])]);
    let v = pair(t_with("REAL"), count(xs("REAL")), count(project(filter(scan(0), lit("TRUE", "BOOLEAN")), vec![col(1, "REAL")])));
    assert!(!matches!(v, Verdict::Refused(_)), "{v:?}");
}
