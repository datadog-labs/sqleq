// Unless explicitly stated otherwise all files in this repository are licensed under the
// Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/).
// Copyright 2026-Present Datadog, Inc.

//! `avg` over an integer is a `numeric` (issue #106).
//!
//! Postgres's `avg` over `smallint`, `integer` or `bigint` returns `numeric`: the mean of `{0, 1}` is
//! `0.5`. The lowering typed it like its operand, an INTEGER, so the QED prover reasoned about the
//! mean as an integer and proved `HAVING avg(a) = 0` equivalent to `HAVING avg(a) < 1 AND avg(a) >
//! -1`. It is the IR's REAL now, the type `numeric` lowers to, and it has `numeric`'s `=`, which is
//! not identity: `avg` over `{19999}` prints `19999.0000000000000000` and over `{39998, 0}`
//! `19999.000000000000`. So an operation that can tell two such values apart, a cast to text or a
//! division, reads it as it reads any other `numeric` (`src/equality.rs`).
//!
//! These pin the lowering and run no prover; the pairs under `tests/pairs/aggregates/` run them.

use serde_json::Value;
use sqleq_frontend::{lower_with, lower_with_ddl, CatalogSource, FrontendError};

const T: &str = r#"create table "t" ("id" INTEGER, "a" INTEGER, "b" INTEGER, "s" SMALLINT, "g" BIGINT, "n" NUMERIC);
create table "u" ("id" INTEGER, "a" INTEGER);"#;

const MODES: [CatalogSource; 2] = [CatalogSource::Declared, CatalogSource::InferredSeeded];

fn pair(q0: &str, q1: &str) -> String {
    format!("{T}\n{q0};\n{q1};")
}

/// The IR of `q` against itself, in `src`.
fn lowered(src: CatalogSource, q: &str) -> Value {
    match lower_with(&pair(q, q), src) {
        Ok(v) => v,
        Err(e) => panic!("{src:?}: expected Ok, got {e}\n{q}"),
    }
}

/// The types of the `op` aggregate calls in `v`, in the order the walk meets them.
fn agg_types(v: &Value, op: &str, out: &mut Vec<String>) {
    match v {
        Value::Object(m) => {
            if m.get("operator").and_then(Value::as_str) == Some(op)
                && m.contains_key("ignoreNulls")
            {
                out.push(m["type"].as_str().unwrap_or("").to_string());
            }
            m.values().for_each(|x| agg_types(x, op, out));
        }
        Value::Array(a) => a.iter().for_each(|x| agg_types(x, op, out)),
        _ => {}
    }
}

/// The type of a plan's first output column: a projection's first target, or a group's first key
/// or, with no keys, its first aggregate.
fn first_output(q: &Value) -> String {
    let first = match q.get("group") {
        Some(g) => g["keys"].get(0).unwrap_or(&g["function"][0]),
        None => &q["project"]["target"][0],
    };
    first["type"].as_str().unwrap_or("").to_string()
}

/// The one `op` aggregate of `q`'s plan, and the type of the plan's first output column.
fn agg_and_output(src: CatalogSource, q: &str, op: &str) -> (String, String) {
    let v = lowered(src, q);
    let mut types = Vec::new();
    agg_types(&v["queries"][0], op, &mut types);
    assert_eq!(types.len(), 1, "{src:?}: one {op} call in {q}: {types:?}");
    let out = first_output(&v["queries"][0]);
    (types.remove(0), out)
}

fn refused(q0: &str, q1: &str) {
    for src in MODES {
        match lower_with(&pair(q0, q1), src) {
            Err(FrontendError::Unsupported(m)) => {
                assert!(
                    m.contains("a value of type numeric"),
                    "{src:?}: refused for {m:?}\n{q0}\n{q1}"
                )
            }
            Err(e) => panic!("{src:?}: expected a refusal, got {e}\n{q0}\n{q1}"),
            Ok(_) => panic!("{src:?}: expected a refusal, but it lowered\n{q0}\n{q1}"),
        }
    }
}

fn lowers(q0: &str, q1: &str) {
    for src in MODES {
        if let Err(e) = lower_with(&pair(q0, q1), src) {
            panic!("{src:?}: expected Ok, got {e}\n{q0}\n{q1}");
        }
    }
}

#[test]
fn avg_over_an_integer_is_a_numeric() {
    // On t = {(1, 0), (1, 1)}, `avg(a)` is 0.5 in group 1.
    for src in MODES {
        for arg in [
            r#""a""#,
            r#""s""#,
            r#""g""#,
            r#""a" + 1"#,
            r#""a" / 2"#,
            r#"CASE WHEN "b" > 0 THEN "a" END"#,
            r#"DISTINCT "a""#,
        ] {
            let q = format!(r#"SELECT avg({arg}) FROM "t""#);
            assert_eq!(
                agg_and_output(src, &q, "AVG"),
                ("REAL".into(), "REAL".into()),
                "{src:?}: {q}"
            );
        }
        let q = r#"SELECT avg("a") FILTER (WHERE "b" > 0) FROM "t""#;
        assert_eq!(
            agg_and_output(src, q, "AVG"),
            ("REAL".into(), "REAL".into()),
            "{src:?}: {q}"
        );
        // A scalar subquery carries the type out of its block.
        let q = r#"SELECT (SELECT avg("a") FROM "u") FROM "t" WHERE "id" > 0"#;
        assert_eq!(agg_and_output(src, q, "AVG").1, "REAL", "{src:?}: {q}");
        // Over `numeric` it was a numeric already.
        let q = r#"SELECT avg("n") FROM "t""#;
        assert_eq!(
            agg_and_output(src, q, "AVG"),
            ("REAL".into(), "REAL".into()),
            "{src:?}: {q}"
        );
        // The neighbours keep an integer's type: `sum` over an integer is one (`bigint`, or a
        // `numeric` of scale 0 over `bigint`), and so are `count`, `min` and `max`.
        for (q, op) in [
            (r#"SELECT sum("a") FROM "t""#, "SUM"),
            (r#"SELECT sum("g") FROM "t""#, "SUM"),
            (r#"SELECT count("a") FROM "t""#, "COUNT"),
            (r#"SELECT min("a") FROM "t""#, "MIN"),
            (r#"SELECT max("s") FROM "t""#, "MAX"),
        ] {
            assert_eq!(
                agg_and_output(src, q, op),
                ("INTEGER".into(), "INTEGER".into()),
                "{src:?}: {q}"
            );
        }
    }
    // A column the inferring mode types from its use as `avg`'s argument, and Postgres DDL.
    let q = r#"SELECT avg("a") FROM "t""#;
    assert_eq!(
        agg_and_output(CatalogSource::Inferred, q, "AVG"),
        ("REAL".into(), "REAL".into())
    );
    let ddl = r#"CREATE TABLE t (id int4, a int2, b int8);"#;
    for arg in ["a", "b", "id"] {
        let q = format!("SELECT avg({arg}) FROM t");
        let v = lower_with_ddl(&format!("{q};\n{q};"), ddl, CatalogSource::Declared).unwrap();
        let mut types = Vec::new();
        agg_types(&v["queries"][0], "AVG", &mut types);
        assert_eq!(types, ["REAL"], "{q}");
    }
}

#[test]
fn avg_over_an_integer_is_read_as_a_numeric() {
    // Two plans, so an operation that shows a numeric's scale is refused: where `t`'s `a` holds 19999
    // and `u`'s holds 39998 and 0, both means are 19999, printed `19999.0000000000000000` and
    // `19999.000000000000`.
    for f in [
        r#"CAST(avg("a") AS TEXT)"#,
        r#"avg("a") || ''"#,
        r#"avg("a") / 3"#,
    ] {
        refused(
            &format!(r#"SELECT {f} FROM "t""#),
            &format!(r#"SELECT {f} FROM "u""#),
        );
    }
    refused(
        r#"SELECT CAST((SELECT avg("a") FROM "u") AS TEXT) FROM "t""#,
        r#"SELECT CAST((SELECT avg("a") FROM "t") AS TEXT) FROM "u""#,
    );
    // An `avg` over the means is an `avg` over numerics, which divides too.
    refused(
        r#"SELECT avg("m") FROM (SELECT avg("a") AS "m" FROM "t" GROUP BY "id") AS "s""#,
        r#"SELECT avg("m") FROM (SELECT avg("a") AS "m" FROM "u" GROUP BY "id") AS "s""#,
    );
    // What gives equal results on equal numerics still lowers: comparisons, exact arithmetic, a
    // rounding, a cast to an integer, a comparison with a scalar subquery, `HAVING`.
    for (q0, q1) in [
        (
            r#"SELECT "id" FROM "t" GROUP BY "id" HAVING avg("a") = 0"#,
            r#"SELECT "id" FROM "t" GROUP BY "id" HAVING avg("a") < 1 AND avg("a") > -1"#,
        ),
        (
            r#"SELECT avg("a") * 2 = 1 FROM "t""#,
            r#"SELECT avg("a") <> avg("a") FROM "t""#,
        ),
        (
            r#"SELECT avg("a") + 1 FROM "t""#,
            r#"SELECT 1 + avg("a") FROM "t""#,
        ),
        (
            r#"SELECT round(avg("a"), 2) FROM "t""#,
            r#"SELECT round(avg("a"), 2) FROM "u""#,
        ),
        (
            r#"SELECT CAST(avg("a") AS INTEGER) FROM "t""#,
            r#"SELECT CAST(avg("a") AS INTEGER) FROM "u""#,
        ),
        (
            r#"SELECT "id" FROM "t" WHERE "a" > (SELECT avg("a") FROM "u")"#,
            r#"SELECT "id" FROM "t" WHERE (SELECT avg("a") FROM "u") < "a""#,
        ),
        (
            r#"SELECT coalesce(avg("a"), 0) > 1 FROM "t""#,
            r#"SELECT coalesce(avg("a"), 0) > 1 FROM "u""#,
        ),
    ] {
        lowers(q0, q1);
    }
}
