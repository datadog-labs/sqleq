// Unless explicitly stated otherwise all files in this repository are licensed under the
// Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/).
// Copyright 2026-Present Datadog, Inc.

//! Values that `=` calls equal and that still print differently: `2.0` and `2.00` as numerics, `-0`
//! and `0` as floats, `'1 day'` and `'24 hours'` as intervals, `'{"a": 1.0}'` and `'{"a": 1.00}'` as
//! jsonb, and arrays of them (issue #84).
//!
//! Both provers read the IR's `=` as identity, so from `t.x = u.x` they put `u.x` for `t.x` inside any
//! function. Each "refused" pair here is not equivalent in Postgres, and used to lower to IR a prover
//! proves equal: an operation that can tell two equal values apart -- a cast to text, `concat`,
//! `scale`, `->>`, numeric division, `avg`, `date + interval` -- reads a value of one of these types.
//! Each "lowers" pair reads one only through operations that give equal results on equal values, or
//! through `q_exact_<type>`, a function of the value's spelling. They do not run a prover; they pin
//! the lowering.

use serde_json::Value;
use sqleq_frontend::{lower_with, CatalogSource, FrontendError};

const MODES: [CatalogSource; 2] = [CatalogSource::Declared, CatalogSource::InferredSeeded];

fn pair(ddl: &str, q0: &str, q1: &str) -> String {
    format!("{ddl}\n{q0};\n{q1};")
}

/// Refused in both modes that read the declared schema, as an operation reading a value of type
/// `class` that can tell two equal ones apart.
fn refused(ddl: &str, q0: &str, q1: &str, class: &str) {
    for src in MODES {
        match lower_with(&pair(ddl, q0, q1), src) {
            Err(FrontendError::Unsupported(m)) => {
                assert!(m.contains(&format!("a value of type {class}")), "{src:?}: refused for {m:?}")
            }
            Err(e) => panic!("{src:?}: expected a refusal for {class}, got {e}\n{q0}\n{q1}"),
            Ok(_) => panic!("{src:?}: expected a refusal for {class}, but it lowered\n{q0}\n{q1}"),
        }
    }
}

/// Lowered in both modes; the declared mode's IR.
fn lowers(ddl: &str, q0: &str, q1: &str) -> Value {
    for src in MODES {
        if let Err(e) = lower_with(&pair(ddl, q0, q1), src) {
            panic!("{src:?}: expected Ok, got {e}\n{q0}\n{q1}");
        }
    }
    lower_with(&pair(ddl, q0, q1), CatalogSource::Declared).unwrap()
}

fn collect(v: &Value, out: &mut Vec<Value>) {
    match v {
        Value::Object(m) => {
            if m.get("operator").and_then(Value::as_str).is_some_and(|o| o.starts_with("q_exact_")) {
                out.push(v.clone());
            }
            m.values().for_each(|x| collect(x, out));
        }
        Value::Array(a) => a.iter().for_each(|x| collect(x, out)),
        _ => {}
    }
}

/// The `q_exact_` terms of one query.
fn exact_terms(q: &Value) -> Vec<Value> {
    let mut out = Vec::new();
    collect(q, &mut out);
    out
}

/// `q` against itself lowers to one plan, which computes one thing however `=` is read, and nothing in
/// it is rewritten.
fn one_plan(ddl: &str, q: &str) {
    let v = lowers(ddl, q, q);
    assert_eq!(v["queries"][0], v["queries"][1]);
    assert!(exact_terms(&v["queries"][0]).is_empty(), "nothing is rewritten in one plan: {q}");
}

const NUMERIC: &str = r#"create table "t" ("x" NUMERIC); create table "u" ("x" NUMERIC);"#;
const FLOAT: &str = r#"create table "t" ("x" DOUBLE PRECISION); create table "u" ("x" DOUBLE PRECISION);"#;
const INTERVAL: &str = r#"create table "t" ("d" DATE, "x" INTERVAL); create table "u" ("d" DATE, "x" INTERVAL);"#;
const JSONB: &str = r#"create table "t" ("x" JSONB); create table "u" ("x" JSONB);"#;

/// `SELECT <f(t.x)> ...` against `SELECT <f(u.x)> ...` under `t.x = u.x`, `%C%` standing for the column.
fn join_pair(f: &str) -> (String, String) {
    let q = |side: &str| format!(r#"SELECT {} FROM "t" JOIN "u" ON "t"."x" = "u"."x""#, f.replace("%C%", side));
    (q(r#""t"."x""#), q(r#""u"."x""#))
}

#[test]
fn a_numeric_is_not_substituted_inside_an_operation_that_shows_its_scale() {
    // Over t = {(2.0)}, u = {(2.00)}: '2.0' against '2.00', and scale 1 against 2.
    for f in [
        "CAST(%C% AS TEXT)",
        "concat(%C%, '')",
        "format('%s', %C%)",
        "scale(%C%)",
        "length(CAST(%C% AS TEXT))",
        "CAST(coalesce(%C%, 0) AS TEXT)",
        "CAST(CASE WHEN %C% > 0 THEN %C% END AS TEXT)",
        // Numeric division and `avg` round to a scale that follows their operands'.
        "%C% / 3",
    ] {
        let (q0, q1) = join_pair(f);
        refused(NUMERIC, &q0, &q1, "numeric");
    }
    let (q0, q1) = join_pair("AVG(%C%)");
    refused(NUMERIC, &q0, &q1, "numeric");
    // From `x = 2`, the constant for the column.
    refused(
        NUMERIC,
        r#"SELECT CAST("x" AS TEXT) FROM "t" WHERE "x" = 2"#,
        r#"SELECT CAST(2 AS TEXT) FROM "t" WHERE "x" = 2"#,
        "numeric",
    );
    // What gives equal results on equal numerics still lowers: a comparison, exact arithmetic, a sum,
    // a rounding, a cast to an integer, coalesce under a comparison.
    for f in ["%C% + 1 > 2", "%C% * 2", "round(%C%, 1)", "CAST(%C% AS INTEGER)", "coalesce(%C%, 0) = 1", "abs(%C%) < 3"] {
        let (q0, q1) = join_pair(f);
        lowers(NUMERIC, &q0, &q1);
    }
    let (q0, q1) = join_pair("SUM(%C%)");
    lowers(NUMERIC, &q0, &q1);
    one_plan(NUMERIC, r#"SELECT "x" / 3 FROM "t""#);
}

#[test]
fn a_float_is_not_substituted_inside_a_cast_to_text() {
    // Over t = {('-0')}, u = {(0)}: '-0' against '0'.
    let (q0, q1) = join_pair("CAST(%C% AS TEXT)");
    refused(FLOAT, &q0, &q1, "float");
    let (q0, q1) = join_pair("CAST(coalesce(%C%, 0) AS TEXT)");
    refused(FLOAT, &q0, &q1, "float");
    refused(
        FLOAT,
        r#"SELECT CAST("x" AS TEXT) FROM "t" WHERE "x" = 0"#,
        r#"SELECT CAST(0 AS TEXT) FROM "t" WHERE "x" = 0"#,
        "float",
    );
    // Float arithmetic and comparisons give equal results on -0 and 0.
    let (q0, q1) = join_pair("%C% * 2.0 + 1 > 3");
    let v = lowers(FLOAT, &q0, &q1);
    // The emitted IR still calls a float VARBINARY: the name that says it is one stays inside.
    assert_eq!(v["schemas"][0]["types"][0], "VARBINARY");
    assert!(!v.to_string().contains("VARBINARY:"), "an internal name leaked: {v}");
    one_plan(FLOAT, r#"SELECT CAST("x" AS TEXT) FROM "t""#);
}

#[test]
fn an_interval_is_not_substituted_where_its_spelling_shows() {
    // '1 day' = '24 hours' and their text differs; '1 mon' = '30 days' and a date plus each differs.
    let (q0, q1) = join_pair("CAST(%C% AS TEXT)");
    refused(INTERVAL, &q0, &q1, "interval");
    let (q0, q1) = join_pair(r#""t"."d" + %C%"#);
    refused(INTERVAL, &q0, &q1, "interval");
    refused(
        INTERVAL,
        r#"SELECT CAST("x" AS TEXT) FROM "t" WHERE "x" = INTERVAL '1 day'"#,
        r#"SELECT CAST(INTERVAL '1 day' AS TEXT) FROM "t" WHERE "x" = INTERVAL '1 day'"#,
        "interval",
    );
    // Comparisons still lower.
    let (q0, q1) = join_pair("%C% > INTERVAL '1 hour'");
    lowers(INTERVAL, &q0, &q1);
}

#[test]
fn a_jsonb_is_not_substituted_where_its_numbers_print() {
    // jsonb = compares numbers by value: '{"a": 1.0}' = '{"a": 1.00}', and their text differs.
    for f in ["CAST(%C% AS TEXT)", "%C% ->> 'a'", "jsonb_extract_path_text(%C%, 'a')"] {
        let (q0, q1) = join_pair(f);
        refused(JSONB, &q0, &q1, "jsonb");
    }
    // `->`, `@>` and a null test read a jsonb as its `=` does.
    for f in ["%C% -> 'a' IS NULL", r#"%C% @> '{"a": 1}'"#] {
        let (q0, q1) = join_pair(f);
        lowers(JSONB, &q0, &q1);
    }
    one_plan(JSONB, r#"SELECT "x" ->> 'a' FROM "t""#);
}

#[test]
fn an_array_compares_its_elements_with_their_equality() {
    // '{2.0}' = '{2.00}' as numeric[], and their text differs; `ARRAY[x]` builds one.
    let ddl = r#"create table "t" ("id" INTEGER, "a" NUMERIC[], "b" NUMERIC[], "n" NUMERIC);"#;
    refused(
        ddl,
        r#"SELECT CAST("a" AS TEXT) FROM "t" WHERE "a" = "b""#,
        r#"SELECT CAST("b" AS TEXT) FROM "t" WHERE "a" = "b""#,
        "numeric[]",
    );
    // (The inferring modes refuse a cast over `ARRAY[..]` before this.)
    match lower_with(
        &pair(
            ddl,
            r#"SELECT CAST(ARRAY["n"] AS TEXT) FROM "t" WHERE "n" = 2"#,
            r#"SELECT CAST(ARRAY[2] AS TEXT) FROM "t" WHERE "n" = 2"#,
        ),
        CatalogSource::Declared,
    ) {
        Err(FrontendError::Unsupported(m)) => assert!(m.contains("a value of type numeric[]"), "{m}"),
        other => panic!("expected a refusal, got {other:?}"),
    }
    // An element of one is a numeric.
    refused(
        ddl,
        r#"SELECT CAST("a"[1] AS TEXT) FROM "t" WHERE "a" = "b""#,
        r#"SELECT CAST("b"[1] AS TEXT) FROM "t" WHERE "a" = "b""#,
        "numeric",
    );
}

#[test]
fn a_numeric_does_not_hide_behind_a_query_block() {
    // Over t = {(2.0)}, u = {(2.00)}: coalesce() is opaque to the provers, and its result reached the
    // outer cast as a plain opaque column.
    refused(
        NUMERIC,
        r#"SELECT CAST("a"."c" AS TEXT) FROM (SELECT coalesce("x", 0) AS "c" FROM "t") AS "a" JOIN (SELECT coalesce("x", 0) AS "c" FROM "u") AS "b" ON "a"."c" = "b"."c""#,
        r#"SELECT CAST("b"."c" AS TEXT) FROM (SELECT coalesce("x", 0) AS "c" FROM "t") AS "a" JOIN (SELECT coalesce("x", 0) AS "c" FROM "u") AS "b" ON "a"."c" = "b"."c""#,
        "numeric",
    );
    // A UNION ALL column takes its first branch's type, and a VALUES column its first row's: a numeric
    // under an integer's name would be read as one.
    let ddl = r#"create table "t" ("i" INTEGER, "n" NUMERIC);"#;
    for (q0, q1) in [
        (r#"SELECT "i" FROM "t" UNION ALL SELECT "n" FROM "t""#, r#"SELECT 1 FROM "t""#),
        (r#"SELECT "c" FROM (VALUES (1), (2.5)) AS "v" ("c")"#, r#"SELECT 1 FROM "t""#),
    ] {
        match lower_with(&pair(ddl, q0, q1), CatalogSource::Declared) {
            Err(FrontendError::Unsupported(m)) => assert!(
                m.contains("of type INTEGER and REAL") || m.contains("column of type INTEGER holding a REAL"),
                "{m}"
            ),
            other => panic!("expected a refusal, got {other:?}"),
        }
    }
    // With the numeric branch first, the column is a numeric, and a read of it is sorted as one.
    lowers(ddl, r#"SELECT "n" FROM "t" UNION ALL SELECT "i" FROM "t""#, r#"SELECT 1 FROM "t""#);
}

#[test]
fn a_value_fixed_by_its_spelling_is_read_through_it() {
    // An interval literal added to a timestamp: the pair still lowers, and the literal is read through
    // q_exact_interval.
    let ddl = r#"create table "t" ("id" INTEGER, "a" INTEGER, "ts" TIMESTAMP, "x" INTERVAL, "d" DATE);"#;
    let v = lowers(
        ddl,
        r#"SELECT "ts" + INTERVAL '1 day' FROM "t" WHERE "a" = 1"#,
        r#"SELECT "ts" + INTERVAL '1 day' FROM "t" WHERE "a" = 1 AND TRUE"#,
    );
    let (e0, e1) = (exact_terms(&v["queries"][0]), exact_terms(&v["queries"][1]));
    assert_eq!(e0.len(), 1, "{e0:?}");
    assert_eq!(e0, e1, "one spelling, one term on both sides");
    assert_eq!(e0[0]["operator"], "q_exact_interval");
    // A filter that makes two literals equal does not make them one term: '1 mon' and '30 days' add
    // differently to 2024-01-31.
    let v = lowers(
        ddl,
        r#"SELECT "d" + INTERVAL '1 mon' FROM "t" WHERE "x" = INTERVAL '1 mon' AND "x" = INTERVAL '30 days'"#,
        r#"SELECT "d" + INTERVAL '30 days' FROM "t" WHERE "x" = INTERVAL '1 mon' AND "x" = INTERVAL '30 days'"#,
    );
    let (e0, e1) = (exact_terms(&v["queries"][0]), exact_terms(&v["queries"][1]));
    assert_eq!((e0.len(), e1.len()), (1, 1));
    assert_ne!(e0, e1);
    // An integer divided by a decimal literal: both operands are fixed by their spelling, the integer
    // column passed in as a value.
    let v = lowers(ddl, r#"SELECT "a" / 2.0 FROM "t""#, r#"SELECT "a" / 2.0 FROM "t" WHERE TRUE"#);
    let e = exact_terms(&v["queries"][0]);
    assert_eq!(e.len(), 2, "{e:?}");
    assert!(e.iter().any(|t| t["operand"][1]["column"] == 1), "the integer is an input: {e:?}");
}
