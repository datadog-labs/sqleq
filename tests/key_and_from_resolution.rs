// Unless explicitly stated otherwise all files in this repository are licensed under the
// Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/).
// Copyright 2026-Present Datadog, Inc.

//! Name resolution for the items of a comma-separated `FROM`.
//!
//! Each "not identical" or "refused" test is a pair that is **not** equivalent in Postgres and used
//! to lower to byte-identical IR, or to IR that reads a name from the wrong table. Each has a
//! control beside it that keeps an equivalent spelling lowering alike. They do not run a prover;
//! they pin the lowering.

use serde_json::Value;
use sqleq_frontend::{lower_with, CatalogSource, FrontendError};

/// The first node anywhere in `v` that carries the field `key`.
fn find_with_field<'a>(v: &'a Value, key: &str) -> Option<&'a Value> {
    if v.get(key).is_some() {
        return Some(v);
    }
    match v {
        Value::Object(m) => m.values().find_map(|x| find_with_field(x, key)),
        Value::Array(a) => a.iter().find_map(|x| find_with_field(x, key)),
        _ => None,
    }
}

// --- Comma-separated FROM items ----------------------------------------------------------------

const ABC: &str = r#"
create table "a" ("id" INTEGER, "x" INTEGER);
create table "b" ("id" INTEGER, "x" INTEGER);
create table "c" ("id" INTEGER, "x" INTEGER, "z" INTEGER);
create table "o" ("id" INTEGER, "x" INTEGER);
"#;

fn abc(q0: &str, q1: &str) -> Result<Value, FrontendError> {
    lower_with(&format!("{ABC}\n{q0};\n{q1};"), CatalogSource::Declared)
}

fn abc_identical(q0: &str, q1: &str) -> bool {
    let v = abc(q0, q1).unwrap_or_else(|e| panic!("expected Ok, got {e}"));
    v["queries"][0] == v["queries"][1]
}

/// `a = {}`, `b = {(2, 1)}`, `c = {(3, 1, 0)}`: the comma binds loosest, so the first crosses the
/// empty `a` with `b RIGHT JOIN c` and returns nothing, where `(a CROSS JOIN b) RIGHT JOIN c` keeps
/// `c`'s row. The same for `FULL`.
#[test]
fn an_outer_join_after_a_comma_item_is_not_grouped_with_it() {
    for kind in ["RIGHT", "FULL"] {
        assert!(
            !abc_identical(
                &format!(r#"SELECT "c"."id" FROM "a", "b" {kind} JOIN "c" ON "b"."x" = "c"."x""#),
                &format!(r#"SELECT "c"."id" FROM "a" CROSS JOIN "b" {kind} JOIN "c" ON "b"."x" = "c"."x""#),
            ),
            "{kind}"
        );
        let v = abc(
            &format!(r#"SELECT "c"."id" FROM "a", "b" {kind} JOIN "c" ON "b"."x" = "c"."x""#),
            r#"SELECT 1 FROM "a""#,
        )
        .unwrap();
        // A cross join of `a` with the outer join, whose condition numbers `b` from 0.
        let top = &find_with_field(&v["queries"][0], "join").unwrap()["join"];
        assert_eq!(top["kind"], "INNER");
        assert_eq!(top["right"]["join"]["kind"], kind);
        let cond = &top["right"]["join"]["condition"]["operand"];
        assert_eq!((cond[0]["column"].as_u64(), cond[1]["column"].as_u64()), (Some(1), Some(3)));
    }
}

/// `a = {(1, 1)}`, `b = {(2, 2)}`, `c = {(3, 1, 0)}`: in the first `USING (x)` joins `b` and `c`
/// and nothing matches; in the second it joins `a` and `c`. The name used to be found on `a` first.
#[test]
fn using_after_a_comma_item_looks_only_at_its_own_join() {
    assert!(!abc_identical(
        r#"SELECT "a"."id", "b"."id", "c"."id" FROM "a", "b" JOIN "c" USING ("x")"#,
        r#"SELECT "a"."id", "b"."id", "c"."id" FROM "a" JOIN "c" USING ("x"), "b""#,
    ));
    // `b.x = c.x`, numbered inside `b JOIN c`; it used to be `a.x = c.x`.
    let v = abc(r#"SELECT 1 FROM "a", "b" JOIN "c" USING ("x")"#, r#"SELECT 1 FROM "a""#).unwrap();
    let top = &find_with_field(&v["queries"][0], "join").unwrap()["join"];
    let cond = &top["right"]["join"]["condition"]["operand"];
    assert_eq!((cond[0]["column"].as_u64(), cond[1]["column"].as_u64()), (Some(1), Some(3)));
}

/// `a` is not visible in the `ON` of `b JOIN c`, so the bare `x` there is the enclosing query's
/// `o.x`, not `a.x`. With `o = {(1, 7)}`, `a = {(10, 5)}`, `b = {(20, 0)}`, `c = {(30, 0, 5)}` the
/// first tests `5 = 7` and the second `5 = 5`.
#[test]
fn an_on_condition_after_a_comma_item_does_not_see_it() {
    assert!(!abc_identical(
        r#"SELECT "o"."id" FROM "o" WHERE EXISTS (SELECT 1 FROM "a", "b" JOIN "c" ON "c"."z" = "x")"#,
        r#"SELECT "o"."id" FROM "o" WHERE EXISTS (SELECT 1 FROM "a", "b" CROSS JOIN "c" WHERE "c"."z" = "a"."x")"#,
    ));
    // Without the enclosing query there is nothing to resolve it to; Postgres rejects it too.
    match abc(r#"SELECT 1 FROM "a", "b" JOIN "c" ON "c"."z" = "a"."x""#, r#"SELECT 1 FROM "a""#) {
        Err(FrontendError::Schema(m)) => assert!(m.contains("a.x"), "{m}"),
        other => panic!("expected an unresolved-column refusal, got {other:?}"),
    }
}

/// The comma items' own joins keep the shape they had: each item is a join input, and inner joins
/// and plain products lower as before.
#[test]
fn comma_items_without_outer_joins_keep_their_shape() {
    assert!(abc_identical(
        r#"SELECT "a"."id" FROM "a", "b", "c""#,
        r#"SELECT "a"."id" FROM "a" CROSS JOIN "b" CROSS JOIN "c""#,
    ));
    assert!(abc_identical(
        r#"SELECT "a"."id" FROM "a" JOIN "b" ON "a"."x" = "b"."x", "c""#,
        r#"SELECT "a"."id" FROM "a" JOIN "b" ON "a"."x" = "b"."x" CROSS JOIN "c""#,
    ));
}

/// Postgres raises "common column name x appears more than once in left table" when the left side
/// of a `USING` has two `x` columns: there is no one column to compare. It used to take the first.
#[test]
fn a_using_name_twice_on_one_side_is_refused() {
    match abc(r#"SELECT 1 FROM "a" JOIN "b" ON TRUE JOIN "c" USING ("x")"#, r#"SELECT 1 FROM "a""#) {
        Err(FrontendError::Schema(m)) => assert!(m.contains("appears more than once on the left"), "{m}"),
        other => panic!("expected the common-column refusal, got {other:?}"),
    }
    match abc(r#"SELECT 1 FROM "c" JOIN ("a" JOIN "b" ON TRUE) USING ("x")"#, r#"SELECT 1 FROM "a""#) {
        Err(FrontendError::Schema(m)) => assert!(m.contains("appears more than once on the right"), "{m}"),
        other => panic!("expected the common-column refusal, got {other:?}"),
    }
    // A name an earlier `USING` merged is one column, so a chain of them still lowers.
    abc(r#"SELECT 1 FROM "a" JOIN "b" USING ("x") JOIN "c" USING ("x")"#, r#"SELECT 1 FROM "a""#).unwrap();
    abc(r#"SELECT 1 FROM "c" JOIN ("a" JOIN "b" USING ("x")) USING ("x")"#, r#"SELECT 1 FROM "a""#).unwrap();
}
