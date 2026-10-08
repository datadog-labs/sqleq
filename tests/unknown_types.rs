// Unless explicitly stated otherwise all files in this repository are licensed under the
// Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/).
// Copyright 2026-Present Datadog, Inc.

//! Values whose type the frontend does not know, and types whose `=` is not even transitive
//! (issue #108).
//!
//! `src/equality.rs` refuses an operation that can tell apart two values `=` calls equal, a cast to
//! text above all, and it used to do so only for the four types it knew to be such (`numeric`, the
//! floats, `interval`, `jsonb`). Every other type was read as one whose `=` is identity: a type no
//! reader names (`numrange`, whose bounds compare as `numeric`s), a domain over `numeric`, the result
//! of a function nobody declared (`round(i, 1)` is a `numeric`, `sqrt(i)` a float). And `box`,
//! `circle`, `lseg` and `line` compare within a tolerance, so their `=` is not transitive and no
//! reading of their values as classes is faithful. Each "refused" pair here is not equivalent in
//! Postgres, and used to lower to IR the QED prover proves equal. Each "lowers" pair keeps a type
//! whose `=` is identity, or reads a value of an unknown type only by comparing it. They do not run
//! a prover; they pin the lowering.

use serde_json::Value;
use sqleq_frontend::{lower_with, lower_with_ddl, CatalogSource, FrontendError};

const MODES: [CatalogSource; 2] = [CatalogSource::Declared, CatalogSource::InferredSeeded];

fn pair(ddl: &str, q0: &str, q1: &str) -> String {
    format!("{ddl}\n{q0};\n{q1};")
}

/// The refusal's message, or a panic naming what happened instead.
fn refusal(lowered: Result<Value, FrontendError>, what: &str) -> String {
    match lowered {
        Err(FrontendError::Unsupported(m)) => m,
        Err(e) => panic!("{what}: expected a refusal, got {e}"),
        Ok(v) => panic!("{what}: expected a refusal, but it lowered to {v}"),
    }
}

/// Refused in both modes that read the declared schema, with a message containing `why`.
fn refused(ddl: &str, q0: &str, q1: &str, why: &str) {
    for src in MODES {
        let m = refusal(lower_with(&pair(ddl, q0, q1), src), &format!("{src:?}\n{q0}\n{q1}"));
        assert!(m.contains(why), "{src:?}: refused for {m:?}, expected {why:?}\n{q0}\n{q1}");
    }
}

/// Refused under the raw Postgres DDL `ddl`, with a message containing `why`.
fn refused_raw(ddl: &str, q0: &str, q1: &str, why: &str) {
    let m = refusal(lower_with_ddl(&format!("{q0};\n{q1};"), ddl, CatalogSource::Declared), &format!("{q0}\n{q1}"));
    assert!(m.contains(why), "refused for {m:?}, expected {why:?}\n{q0}\n{q1}");
}

/// Lowered in both modes; the declared mode's IR, in which no frontend-internal type name is left.
fn lowers(ddl: &str, q0: &str, q1: &str) -> Value {
    for src in MODES {
        if let Err(e) = lower_with(&pair(ddl, q0, q1), src) {
            panic!("{src:?}: expected Ok, got {e}\n{q0}\n{q1}");
        }
    }
    let v = lower_with(&pair(ddl, q0, q1), CatalogSource::Declared).unwrap();
    let text = v.to_string();
    assert!(!text.contains("VARBINARY:") && !text.contains("VARBINARY="), "an internal name leaked: {text}");
    v
}

/// Lowered under the raw Postgres DDL `ddl`.
fn lowers_raw(ddl: &str, q0: &str, q1: &str) -> Value {
    lower_with_ddl(&format!("{q0};\n{q1};"), ddl, CatalogSource::Declared)
        .unwrap_or_else(|e| panic!("expected Ok, got {e}\n{q0}\n{q1}"))
}

fn collect(v: &Value, out: &mut Vec<String>) {
    match v {
        Value::Object(m) => {
            if let Some(op) = m.get("operator").and_then(Value::as_str) {
                out.push(op.to_string());
            }
            m.values().for_each(|x| collect(x, out));
        }
        Value::Array(a) => a.iter().for_each(|x| collect(x, out)),
        _ => {}
    }
}

/// The operators of both queries.
fn operators(v: &Value) -> Vec<String> {
    let mut out = Vec::new();
    collect(&v["queries"], &mut out);
    out
}

/// `SELECT CAST(a AS TEXT) ... WHERE a = b` against the same with `b`: equivalent exactly where `=`
/// on the type of `a` and `b` is identity.
const CAST_A: &str = r#"SELECT CAST("a" AS TEXT) FROM "t" WHERE "a" = "b""#;
const CAST_B: &str = r#"SELECT CAST("b" AS TEXT) FROM "t" WHERE "a" = "b""#;

/// A message the refusal of a value of an unknown type carries.
const UNKNOWN: &str = "= is not known to be identity";

#[test]
fn a_type_no_reader_names_is_not_read_as_one_whose_equality_is_identity() {
    // '[1.0,2.0)' = '[1.00,2.00)' as numranges, and their text differs.
    for ty in ["numrange", "nummultirange", "path", "tsquery", "mood"] {
        let ddl = format!(r#"create table "t" ("id" INTEGER, "a" {ty}, "b" {ty});"#);
        refused(&ddl, CAST_A, CAST_B, &format!("a value of type {ty}"));
        refused_raw(&ddl, CAST_A, CAST_B, UNKNOWN);
    }
    // A `DISTINCT` puts equal values in one class without a written `=`.
    refused(
        r#"create table "t" ("id" INTEGER, "a" numrange);"#,
        r#"SELECT DISTINCT CAST("a" AS TEXT) FROM (SELECT DISTINCT "a" FROM "t") AS "s""#,
        r#"SELECT DISTINCT CAST("a" AS TEXT) FROM "t""#,
        "a value of type numrange",
    );
    // An array of one compares its elements with their `=`.
    refused(r#"create table "t" ("id" INTEGER, "a" numrange[], "b" numrange[]);"#, CAST_A, CAST_B, UNKNOWN);
}

#[test]
fn the_result_of_a_function_nobody_declared_is_not_read_as_identity() {
    let ddl = r#"create table "t" ("id" INTEGER, "i" INTEGER, "j" INTEGER);"#;
    // round(integer, integer) is a numeric: over t = {(1, 1, 1)}, x is 1.0 and y is 1.00.
    let q = |c: &str| {
        format!(
            r#"SELECT CAST("{c}" AS TEXT) FROM (SELECT round("i", 1) AS "x", round("i", 2) AS "y" FROM "t") AS "s" WHERE "x" = "y""#
        )
    };
    refused(ddl, &q("x"), &q("y"), "the result of a function nobody declared");
    // sqrt(integer) is a double precision: over t = {(1, 0, 0)}, x is -0 and y is 0.
    let q = |c: &str| {
        format!(
            r#"SELECT CAST("{c}" AS TEXT) FROM (SELECT -sqrt("i") AS "x", sqrt("j") AS "y" FROM "t") AS "s" WHERE "x" = "y""#
        )
    };
    refused(ddl, &q("x"), &q("y"), "the result of a function nobody declared");
    // Read from a row by any other undeclared function, or by coalesce, which returns it.
    for f in ["upper(%C%)", "CAST(coalesce(%C%, 0) AS TEXT)", "%C% || 'x'", "abs(%C%)", "%C% @> 1"] {
        let q = |c: &str| {
            format!(
                r#"SELECT {} FROM (SELECT round("i", 1) AS "x", round("i", 2) AS "y" FROM "t") AS "s" WHERE "x" = "y""#,
                f.replace("%C%", &format!(r#""{c}""#))
            )
        };
        refused(ddl, &q("x"), &q("y"), "the result of a function nobody declared");
    }
}

#[test]
fn a_value_of_an_unknown_type_is_still_compared_and_computed_in_place() {
    let ddl = r#"create table "t" ("id" INTEGER, "i" INTEGER, "j" INTEGER, "r" numrange);"#;
    // A comparison, a join, a null test, an order, `count` and `max` read the value by its class.
    lowers(
        ddl,
        r#"SELECT "id" FROM "t" WHERE round("i", 1) = round("j", 1) AND "r" IS NOT NULL"#,
        r#"SELECT "id" FROM "t" WHERE round("j", 1) = round("i", 1) AND "r" IS NOT NULL"#,
    );
    lowers(
        ddl,
        r#"SELECT "x" FROM (SELECT round("i", 1) AS "x", round("j", 1) AS "y" FROM "t") AS "s" WHERE "x" = "y""#,
        r#"SELECT "y" FROM (SELECT round("i", 1) AS "x", round("j", 1) AS "y" FROM "t") AS "s" WHERE "x" = "y""#,
    );
    lowers(
        ddl,
        r#"SELECT count("r"), max(round("i", 1)) FROM "t" WHERE "r" IS NOT NULL"#,
        r#"SELECT count("r"), max(round("j", 1)) FROM "t" WHERE "r" IS NOT NULL"#,
    );
    // So does a cast to a number or a boolean, which converts by value.
    let q = |c: &str| {
        format!(
            r#"SELECT CAST("{c}" AS INTEGER) FROM (SELECT round("i", 1) AS "x", round("i", 2) AS "y" FROM "t") AS "s" WHERE "x" = "y""#
        )
    };
    lowers(ddl, &q("x"), &q("y"));
    // An observer over a value computed in place from values whose `=` is identity reads it through
    // its spelling, so one spelling over equal inputs is one term.
    let v = lowers(
        ddl,
        r#"SELECT CAST(round("i", 1) AS TEXT) FROM "t" WHERE "i" = "j""#,
        r#"SELECT CAST(round("j", 1) AS TEXT) FROM "t" WHERE "i" = "j""#,
    );
    assert!(operators(&v).iter().any(|o| o == "q_exact_varbinary"), "{v}");
}

#[test]
fn a_type_whose_equality_is_identity_keeps_its_proofs() {
    // `uuid` keeps its own name in a declared `CREATE TABLE`; `bytea` and the arrays of identity
    // types are opaque. A cast to text over any of them is not refused, and nothing is rewritten.
    for ty in ["uuid", "bytea", "INTEGER[]", "TEXT[]", "int4range", "money"] {
        let ddl = format!(r#"create table "t" ("id" INTEGER, "a" {ty}, "b" {ty});"#);
        let v = lowers(&ddl, CAST_A, CAST_B);
        assert!(!operators(&v).iter().any(|o| o.starts_with("q_exact_")), "{ty}: {v}");
        let v = lowers_raw(&ddl, CAST_A, CAST_B);
        assert!(!operators(&v).iter().any(|o| o.starts_with("q_exact_")), "{ty} (raw DDL): {v}");
    }
    // A function that returns one of its arguments, over values whose `=` is identity, returns one.
    let ddl = r#"create table "t" ("id" INTEGER, "i" INTEGER, "j" INTEGER);"#;
    let q = |c: &str| {
        format!(
            r#"SELECT CAST("{c}" AS TEXT) FROM (SELECT coalesce("i", 0) AS "x", coalesce("j", 0) AS "y" FROM "t") AS "s" WHERE "x" = "y""#
        )
    };
    lowers(ddl, &q("x"), &q("y"));
    // A type with no `=` at all puts no two values in one class, and its JSON lookups are of its
    // type; `->>` is text.
    for raw in [false, true] {
        let ddl = r#"create table "t" ("id" INTEGER, "a" json, "p" point);"#;
        for (q0, q1) in [
            (r#"SELECT upper("x") FROM (SELECT "a" ->> 'k' AS "x" FROM "t") AS "s""#, r#"SELECT upper("a" ->> 'k') FROM "t""#),
            (r#"SELECT CAST("x" AS TEXT) FROM (SELECT "a" -> 'k' AS "x" FROM "t") AS "s""#, r#"SELECT CAST("a" -> 'k' AS TEXT) FROM "t""#),
            (r#"SELECT CAST("a" AS TEXT), CAST("p" AS TEXT) FROM "t" WHERE "id" = 1"#, r#"SELECT CAST("a" AS TEXT), CAST("p" AS TEXT) FROM "t" WHERE 1 = "id""#),
        ] {
            let v = if raw { lowers_raw(ddl, q0, q1) } else { lowers(ddl, q0, q1) };
            assert!(!operators(&v).iter().any(|o| o.starts_with("q_exact_")), "{q0} (raw DDL: {raw}): {v}");
        }
    }
    // An array built of them. (The inferring modes refuse a cast over `ARRAY[..]` before this.)
    let ddl = r#"create table "t" ("id" INTEGER, "i" INTEGER, "j" INTEGER, "p" point, "q" point);"#;
    let v = lower_with(
        &pair(
            ddl,
            r#"SELECT CAST(ARRAY["i"] AS TEXT) FROM "t" WHERE "i" = "j""#,
            r#"SELECT CAST(ARRAY["j"] AS TEXT) FROM "t" WHERE "i" = "j""#,
        ),
        CatalogSource::Declared,
    )
    .unwrap_or_else(|e| panic!("expected Ok, got {e}"));
    assert!(!operators(&v).iter().any(|o| o.starts_with("q_exact_")), "{v}");
    // A subscript need not yield a value of the same kind: a point has no `=`, and p[0] is a float
    // (over p = (-0,0) and q = (0,0), x = y holds and the two texts are -0 and 0).
    let q = |c: &str| format!(r#"SELECT CAST("{c}" AS TEXT) FROM (SELECT "p"[0] AS "x", "q"[0] AS "y" FROM "t") AS "s" WHERE "x" = "y""#);
    refused(ddl, &q("x"), &q("y"), UNKNOWN);
}

#[test]
fn a_geometric_type_whose_equality_is_not_transitive_is_refused() {
    // Over boxes of area 1, 1.0000009 and 1.0000018, a = b and b = c hold and a = c does not.
    for ty in ["box", "circle", "lseg", "line"] {
        let ddl = format!(r#"create table "t" ("id" INTEGER, "a" {ty}, "b" {ty}, "c" {ty});"#);
        let q0 = r#"SELECT "id" FROM "t" WHERE "a" = "b" AND "b" = "c""#;
        let q1 = r#"SELECT "id" FROM "t" WHERE "a" = "b" AND "b" = "c" AND "a" = "c""#;
        refused(&ddl, q0, q1, &format!("a value of type {ty}"));
        refused_raw(&ddl, q0, q1, &format!("a value of type {ty}"));
        refused(&ddl, CAST_A, CAST_B, &format!("a value of type {ty}"));
        // Arrays of one, and a cast to one.
        let arr = format!(r#"create table "t" ("id" INTEGER, "a" {ty}[], "b" {ty}[]);"#);
        refused(&arr, CAST_A, CAST_B, &format!("a value of type {ty}"));
    }
    // A core function that returns a box returns one, whether or not anyone declared it.
    let ddl = r#"create table "t" ("id" INTEGER, "p" point, "q" point, "r" point);"#;
    refused(
        ddl,
        r#"SELECT "id" FROM "t" WHERE box("p", "p") = box("p", "q") AND box("p", "q") = box("p", "r")"#,
        r#"SELECT "id" FROM "t" WHERE box("p", "p") = box("p", "q") AND box("p", "q") = box("p", "r") AND box("p", "p") = box("p", "r")"#,
        "a value of type box",
    );
    refused(
        ddl,
        r#"SELECT "id" FROM "t" WHERE CAST("p" AS box) = CAST("q" AS box)"#,
        r#"SELECT "id" FROM "t" WHERE CAST("q" AS box) = CAST("p" AS box)"#,
        "a value of type box",
    );
    // One plan computes one thing however `=` is read.
    let ddl = r#"create table "t" ("id" INTEGER, "a" box, "b" box);"#;
    let v = lowers(ddl, CAST_A, CAST_A);
    assert_eq!(v["queries"][0], v["queries"][1]);
}

#[test]
fn a_domain_is_its_base_type() {
    // A domain over numeric is a numeric: over t = {(1, 2.0, 2.00)}, A yields 2.0 and B 2.00.
    for raw in [false, true] {
        let ddl = r#"create domain "d" as numeric; create table "t" ("id" INTEGER, "a" d, "b" d);"#;
        if raw {
            refused_raw(ddl, CAST_A, CAST_B, "a value of type numeric");
        } else {
            refused(ddl, CAST_A, CAST_B, "a value of type numeric");
        }
    }
    refused(
        r#"create domain "f" as double precision; create table "t" ("id" INTEGER, "a" f, "b" f);"#,
        CAST_A,
        CAST_B,
        "a value of type float",
    );
    // A domain over text, or over integer through a second domain, is that type, and so lowers, with
    // the CHECK it adds, as its base type would.
    let ddl = r#"create domain "e" as text check (value <> ''); create domain "n" as integer;
        create domain "m" as n; create table "t" ("id" INTEGER, "a" e, "b" e, "k" m);"#;
    let v = lowers(ddl, CAST_A, CAST_B);
    assert_eq!(v["schemas"][0]["types"], serde_json::json!(["INTEGER", "VARCHAR", "VARCHAR", "INTEGER"]));
    let v = lowers_raw(ddl, CAST_A, CAST_B);
    assert_eq!(v["schemas"][0]["types"], serde_json::json!(["INTEGER", "VARCHAR", "VARCHAR", "INTEGER"]));
    // Not read as a domain, and so a type the frontend does not know: one with a collation, whose
    // `=` need not be its base type's; one created twice; one named like a type Postgres predefines,
    // which Postgres finds first.
    for ddl in [
        r#"create domain "d" as text collate "C"; create table "t" ("id" INTEGER, "a" d, "b" d);"#,
        r#"create domain "d" as text; create domain "d" as numeric; create table "t" ("id" INTEGER, "a" d, "b" d);"#,
        r#"create domain "numrange" as text; create table "t" ("id" INTEGER, "a" numrange, "b" numrange);"#,
    ] {
        refused(ddl, CAST_A, CAST_B, UNKNOWN);
    }
}

#[test]
fn a_set_operation_does_not_hide_an_unknown_value_under_an_identity_column() {
    // The column takes INTEGER from its first branch, and holds the numeric round(i, 1) under it.
    let ddl = r#"create table "t" ("id" INTEGER, "i" INTEGER, "ts" TIMESTAMP);"#;
    refused(
        ddl,
        r#"SELECT CAST("x" AS TEXT) FROM (SELECT "i" AS "x" FROM "t" UNION ALL SELECT round("i", 1) FROM "t") AS "s""#,
        r#"SELECT CAST("x" AS TEXT) FROM (SELECT "i" AS "x" FROM "t") AS "s""#,
        "set operation over columns of type INTEGER and VARBINARY",
    );
    // A timestamp column keeps its type in Postgres whatever else it holds, or the query fails.
    lowers(
        ddl,
        r#"SELECT CAST("x" AS TEXT) FROM (SELECT "ts" AS "x" FROM "t" UNION ALL SELECT date_trunc('day', "ts") FROM "t") AS "s""#,
        r#"SELECT CAST("x" AS TEXT) FROM (SELECT "ts" AS "x" FROM "t") AS "s""#,
    );
}
