// Unless explicitly stated otherwise all files in this repository are licensed under the
// Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/).
// Copyright 2026-Present Datadog, Inc.

//! A value coerced into an opaque type is read at the type it is coerced into (issue #154).
//!
//! Every opaque type is VARBINARY to the provers, so a literal coerced into a `uuid` column and the
//! same literal coerced into a `name` column used to be one term, `CAST('x' AS VARBINARY)`. With
//! `u = 'x' AND n = 'x'` the provers then had `u = n`, and `SELECT u` was `SELECT n`, though one
//! returns a uuid and the other a name. The coercion is now the cast to the column's declared type.
//! Each test reads its DDL from a pair file and as raw DDL, and runs no prover.

use std::collections::BTreeSet;

use serde_json::Value;
use sqleq_frontend::{lower_with, lower_with_ddl, CatalogSource, Result};

/// The pair `q0`/`q1` over `ddl`, lowered from a pair file and from raw DDL.
fn both(ddl: &str, q0: &str, q1: &str) -> [Result<Value>; 2] {
    [
        lower_with(&format!("{ddl}\n{q0};\n{q1};"), CatalogSource::Declared),
        lower_with_ddl(&format!("{q0};\n{q1};"), ddl, CatalogSource::Declared),
    ]
}

fn lowered(ddl: &str, q0: &str, q1: &str) -> [Value; 2] {
    both(ddl, q0, q1).map(|r| r.unwrap_or_else(|e| panic!("expected Ok, got {e}\n{ddl}\n{q0}\n{q1}")))
}

/// Every operator name in `v`.
fn operators(v: &Value) -> BTreeSet<String> {
    fn walk(v: &Value, out: &mut BTreeSet<String>) {
        match v {
            Value::Object(m) => {
                if let Some(Value::String(op)) = m.get("operator") {
                    out.insert(op.clone());
                }
                m.values().for_each(|x| walk(x, out));
            }
            Value::Array(a) => a.iter().for_each(|x| walk(x, out)),
            _ => {}
        }
    }
    let mut out = BTreeSet::new();
    walk(v, &mut out);
    out
}

/// The cast functions a lowered pair applies.
fn casts(v: &Value) -> BTreeSet<String> {
    operators(v).into_iter().filter(|o| o.starts_with("q_cast_")).collect()
}

/// Two columns of the types `a` and `b`, each compared with `lit`; one query returns each column.
fn two_columns(a: &str, b: &str, lit: &str) -> (String, String, String) {
    let ddl = format!(r#"create table "t" ("id" INTEGER PRIMARY KEY, "a" {a}, "b" {b});"#);
    let filter = format!(r#"WHERE "a" = {lit} AND "b" = {lit}"#);
    (ddl, format!(r#"SELECT "a" FROM "t" {filter}"#), format!(r#"SELECT "b" FROM "t" {filter}"#))
}

#[test]
fn a_literal_compared_with_two_opaque_types_is_two_values() {
    // Each pair of types reads the literal as two values that print differently, checked on
    // Postgres 17: 'A0EE…' as a uuid is lower case and as a name stays as written; '[1,2]' is
    // [1,3) as an int4range and [1,2] as a numrange; '1.0' is 1 as a float8 and 1.0 as a jsonb;
    // '16777217' is 16777216 as a real and 16777217 as a double precision.
    let uuid = "'A0EEBC99-9C0B-4EF8-BB6D-6BB9BD380A11'";
    for (a, b, lit) in [
        ("uuid", "name", uuid),
        ("uuid", "bytea", "'00000000-0000-0000-0000-000000000000'"),
        ("int4range", "numrange", "'[1,2]'"),
        ("float8", "jsonb", "'1.0'"),
        ("real", "double precision", "'16777217'"),
    ] {
        let (ddl, q0, q1) = two_columns(a, b, lit);
        for (mode, v) in ["pair file", "raw DDL"].into_iter().zip(lowered(&ddl, &q0, &q1)) {
            assert_ne!(v["queries"][0], v["queries"][1], "{mode}, {a} and {b}: one plan\n{v}");
            // Where the IR keeps neither type's name, the two coercions are two cast functions.
            let c = casts(&v);
            let emitted = v["schemas"][0]["types"].as_array().unwrap();
            if emitted[1] == emitted[2] {
                assert_eq!(c.len(), 2, "{mode}, {a} and {b}: expected two cast functions, got {c:?}\n{v}");
            }
        }
    }
}

#[test]
fn a_literal_compared_with_one_opaque_type_is_one_value() {
    // Two columns of one type read the literal as one value, so `a = 'x' AND b = 'x'` still gives
    // `a = b`, as it does in Postgres.
    for (ty, lit) in [("uuid", "'A0EEBC99-9C0B-4EF8-BB6D-6BB9BD380A11'"), ("double precision", "'1.5'")] {
        let (ddl, q0, q1) = two_columns(ty, ty, lit);
        let [_, raw] = lowered(&ddl, &q0, &q1);
        assert_eq!(casts(&raw).len(), 1, "{ty}: expected one cast function, got {:?}\n{raw}", casts(&raw));
    }
}

#[test]
fn a_literal_compared_with_a_column_is_cast_to_its_declared_type() {
    // Under raw DDL a `uuid` column is opaque, and `u = 'x'` reads 'x' through the cast named after
    // the column's declared type.
    let ddl = r#"create table "t" ("id" INTEGER PRIMARY KEY, "u" uuid);"#;
    let q = r#"SELECT "id" FROM "t" WHERE "u" = 'A0EEBC99-9C0B-4EF8-BB6D-6BB9BD380A11'"#;
    let [_, raw] = lowered(ddl, q, q);
    assert_eq!(casts(&raw), BTreeSet::from(["q_cast_uuid".to_string()]), "{raw}");
}

#[test]
fn a_literal_returned_beside_an_opaque_column_is_read_at_its_type() {
    // `coalesce(a, 16777217)` returns 16777216 over a real `a` that is NULL and 16777217 over a
    // double precision one; the literal used to be returned as the integer it is written as.
    let ddl = r#"create table "t" ("id" INTEGER PRIMARY KEY, "a" real, "b" double precision);"#;
    let q0 = r#"SELECT coalesce("a", 16777217) FROM "t" WHERE "a" IS NULL AND "b" IS NULL"#;
    let q1 = r#"SELECT coalesce("b", 16777217) FROM "t" WHERE "a" IS NULL AND "b" IS NULL"#;
    for (mode, v) in ["pair file", "raw DDL"].into_iter().zip(lowered(ddl, q0, q1)) {
        assert_eq!(casts(&v).len(), 2, "{mode}: expected the literal cast to real and to double precision\n{v}");
    }
}

#[test]
fn a_numeric_coalesce_keeps_its_literal() {
    // With no argument of an opaque type, a coalesce is lowered as it was: `numeric` is not opaque,
    // and its literal is read by value.
    let ddl = r#"create table "t" ("id" INTEGER PRIMARY KEY, "x" numeric);"#;
    let q = r#"SELECT coalesce("x", 0) FROM "t""#;
    for v in lowered(ddl, q, q) {
        assert!(casts(&v).is_empty(), "{v}");
    }
}
