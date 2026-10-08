// Unless explicitly stated otherwise all files in this repository are licensed under the
// Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/).
// Copyright 2026-Present Datadog, Inc.

//! A value an `UPDATE` or an `INSERT` stores in a column of another type is stored through
//! Postgres's assignment cast to the column's type (issue #107). `SET s = n`, with `s` text and `n`
//! numeric, stores `n::text`, which tells `2.0` from `2.00` though `2.0 = 2.00`. The DML reductions
//! hand the provers `n` itself, and both read a value as its class under `=`, so under `n = m` they
//! put `m` for `n` in the stored value.
//!
//! Each "refused" pair here is not equivalent in Postgres and used to lower to IR a prover proves
//! equal, or that a prover would. Each "lowers" pair stores such a value only through a cast that
//! converts it by value, or one that reads it through `q_exact_<type>`, a function of its spelling.
//! They do not run a prover; they pin the lowering.

use serde_json::Value;
use sqleq_frontend::{lower_with, lower_with_ddl, CatalogSource, FrontendError};

/// A table with most kinds of column a value can be stored in, a source table, and two more: a
/// `char(n)` column is refused wherever an `UPDATE` projects it, and `x` is the shape of `t (id, s)`.
const DDL: &str = r#"
create table "t" ("id" INTEGER PRIMARY KEY, "s" TEXT, "v" VARCHAR(10), "j" JSON, "jb" JSONB,
    "ta" TEXT[], "na" NUMERIC[], "i" INTEGER, "n" NUMERIC, "m" NUMERIC, "n2" NUMERIC(10,2),
    "f" DOUBLE PRECISION, "g" DOUBLE PRECISION, "iv" INTERVAL, "jv" INTERVAL, "d" INTERVAL DAY,
    "p" INTERVAL(0), "hm" INTERVAL HOUR TO MINUTE, "da" INTERVAL DAY[]);
create table "u" ("k" INTEGER, "n" NUMERIC, "m" NUMERIC, "f" DOUBLE PRECISION, "iv" INTERVAL,
    "jb" JSONB, "s" TEXT);
create table "w" ("id" INTEGER, "c" CHAR(5));
create table "x" ("k" INTEGER, "n" NUMERIC);
"#;

fn pair(q0: &str, q1: &str) -> String {
    format!("{DDL}\n{q0};\n{q1};")
}

/// The refusal of a store of a value of type `class` through a cast that can tell two equal ones
/// apart.
fn is_store_refusal(r: &Result<Value, FrontendError>, class: &str) -> bool {
    matches!(r, Err(FrontendError::Unsupported(m))
        if m.contains(&format!("a value of type {class} read by the assignment cast")))
}

/// Refused under the declared schema for storing a value of type `class` through a cast that can
/// tell two equal ones apart, and not lowered with inferred parameters either. There an `UPDATE`'s
/// `WHERE` can refuse first: inference unifies the two arms of the `CASE` it becomes, and so the
/// stored value's type with the column's, and finds them in conflict.
fn refused(q0: &str, q1: &str, class: &str) {
    let declared = lower_with(&pair(q0, q1), CatalogSource::Declared);
    assert!(is_store_refusal(&declared, class), "expected a refusal for {class}, got {declared:?}\n{q0}\n{q1}");
    if let Ok(v) = lower_with(&pair(q0, q1), CatalogSource::InferredSeeded) {
        panic!("InferredSeeded: expected a refusal for {class}, but it lowered to {v}\n{q0}\n{q1}");
    }
}

/// Lowered under the declared schema; its IR.
fn lowers(q0: &str, q1: &str) -> Value {
    match lower_with(&pair(q0, q1), CatalogSource::Declared) {
        Ok(v) => v,
        Err(e) => panic!("expected Ok, got {e}\n{q0}\n{q1}"),
    }
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

#[test]
fn an_update_does_not_store_a_numeric_as_text_by_its_class() {
    // The issue's pair: over t = {(1, NULL, .., 2.0, 2.00, ..)}, A stores '2.0' and B '2.00'.
    refused(r#"UPDATE t SET s = n WHERE n = m"#, r#"UPDATE t SET s = m WHERE n = m"#, "numeric");
    // Without a WHERE, the value is the whole output rather than the THEN of a CASE.
    refused(r#"UPDATE t SET s = n"#, r#"UPDATE t SET s = m"#, "numeric");
    // varchar(n) and char(n) store the text too, and json stores a jsonb's.
    refused(r#"UPDATE t SET v = n WHERE n = m"#, r#"UPDATE t SET v = m WHERE n = m"#, "numeric");
    refused(r#"UPDATE t SET j = jb WHERE id = 1"#, r#"UPDATE t SET j = jb WHERE id = 1 AND TRUE"#, "jsonb");
    // Every assignment of a multi-column SET is a store of its own, the second one here.
    refused(
        r#"UPDATE t SET i = 1, s = n WHERE n = m"#,
        r#"UPDATE t SET i = 1, s = m WHERE n = m"#,
        "numeric",
    );
    // A value from a subquery is read from a row of another table.
    refused(
        r#"UPDATE t SET s = (SELECT u.n FROM u WHERE u.k = t.id AND u.n = u.m)"#,
        r#"UPDATE t SET s = (SELECT u.m FROM u WHERE u.k = t.id AND u.n = u.m)"#,
        "numeric",
    );
    // A RETURNING list makes the rows the statement matched a second goal, with the same projection.
    refused(
        r#"UPDATE t SET s = n WHERE n = m RETURNING id"#,
        r#"UPDATE t SET s = m WHERE n = m RETURNING id"#,
        "numeric",
    );
    // A FROM that only filters.
    refused(
        r#"UPDATE t SET s = t.n FROM u WHERE u.k = t.id AND t.n = t.m"#,
        r#"UPDATE t SET s = t.m FROM u WHERE u.k = t.id AND t.n = t.m"#,
        "numeric",
    );
}

#[test]
fn an_update_does_not_store_a_float_an_interval_or_an_array_as_text_by_its_class() {
    // -0 and 0; '1 day' and '24 hours'; {2.0} and {2.00}.
    refused(r#"UPDATE t SET s = f WHERE f = g"#, r#"UPDATE t SET s = g WHERE f = g"#, "float");
    refused(r#"UPDATE t SET s = iv WHERE iv = jv"#, r#"UPDATE t SET s = jv WHERE iv = jv"#, "interval");
    refused(
        r#"UPDATE t SET ta = na WHERE id = 1"#,
        r#"UPDATE t SET ta = na WHERE id = 1 AND TRUE"#,
        "numeric[]",
    );
}

#[test]
fn an_insert_does_not_store_a_numeric_as_text_by_its_class() {
    // The issue's second pair: over u = {(.., 2.0, 2.00, ..)}, A inserts '2.0' and B '2.00'.
    refused(
        r#"INSERT INTO t (id, s) SELECT k, n FROM u WHERE n = m"#,
        r#"INSERT INTO t (id, s) SELECT k, m FROM u WHERE n = m"#,
        "numeric",
    );
    // A VALUES cell that reads a row: a scalar subquery.
    refused(
        r#"INSERT INTO t (id, s) VALUES (1, (SELECT n FROM u WHERE n = m AND k = 1))"#,
        r#"INSERT INTO t (id, s) VALUES (1, (SELECT m FROM u WHERE n = m AND k = 1))"#,
        "numeric",
    );
    // Each branch of a UNION ALL stores its own rows.
    refused(
        r#"INSERT INTO t (id, s) SELECT k, 2.0 FROM u UNION ALL SELECT k, n FROM u WHERE n = m"#,
        r#"INSERT INTO t (id, s) SELECT k, 2.0 FROM u UNION ALL SELECT k, m FROM u WHERE n = m"#,
        "numeric",
    );
    // char(n) pads the text, and stores it.
    refused(
        r#"INSERT INTO w (id, c) SELECT k, n FROM u WHERE n = m"#,
        r#"INSERT INTO w (id, c) SELECT k, m FROM u WHERE n = m"#,
        "numeric",
    );
    // Under a DISTINCT, a bare SELECT * or a slice, the stored value is read from a row, and no one
    // expression computes it.
    refused(
        r#"INSERT INTO t (s) SELECT DISTINCT n FROM u"#,
        r#"INSERT INTO t (s) SELECT DISTINCT m FROM u"#,
        "numeric",
    );
    refused(
        r#"INSERT INTO t (id, s) SELECT * FROM x WHERE n = 2"#,
        r#"INSERT INTO t (id, s) SELECT k, 2 FROM x WHERE n = 2"#,
        "numeric",
    );
    refused(
        r#"INSERT INTO t (s) SELECT n FROM u ORDER BY k LIMIT 1"#,
        r#"INSERT INTO t (s) SELECT m FROM u ORDER BY k LIMIT 1"#,
        "numeric",
    );
}

#[test]
fn an_insert_does_not_store_a_parameter_s_equal_by_its_class() {
    // Over u = {(1, 2.0, ..)} and $1 = '2.00': A inserts '2.0', B '2.00'. A bare `$N` needs the
    // inferring mode.
    let r = lower_with(
        &pair(
            r#"INSERT INTO t (id, s) SELECT k, n FROM u WHERE n = $1::numeric"#,
            r#"INSERT INTO t (id, s) SELECT k, $1::numeric FROM u WHERE n = $1::numeric"#,
        ),
        CatalogSource::InferredSeeded,
    );
    assert!(is_store_refusal(&r, "numeric"), "{r:?}");
}

#[test]
fn an_interval_column_with_a_modifier_coerces_by_more_than_the_class() {
    // `interval day` keeps the days: '1 day' stays, '24 hours' becomes 0. `interval(0)` rounds the
    // seconds away from zero, '1 day -00:00:00.5' to '1 day -00:00:01' and '23:59:59.5' to
    // '24:00:00', and `interval hour to minute` truncates the time toward zero.
    for col in ["d", "p", "hm"] {
        refused(
            &format!("UPDATE t SET {col} = iv WHERE iv = jv"),
            &format!("UPDATE t SET {col} = jv WHERE iv = jv"),
            "interval",
        );
    }
    refused(
        r#"INSERT INTO t (id, d) SELECT k, iv FROM u WHERE iv = INTERVAL '1 day'"#,
        r#"INSERT INTO t (id, d) SELECT k, INTERVAL '1 day' FROM u WHERE iv = INTERVAL '1 day'"#,
        "interval",
    );
    refused(
        r#"UPDATE t SET da = ARRAY[iv] WHERE iv = jv"#,
        r#"UPDATE t SET da = ARRAY[jv] WHERE iv = jv"#,
        "interval[]",
    );
    // A plain interval column stores the value it is given.
    lowers(r#"UPDATE t SET iv = jv WHERE iv = jv"#, r#"UPDATE t SET iv = iv WHERE iv = jv"#);
}

#[test]
fn a_raw_ddl_schema_keeps_the_interval_modifier_too() {
    let ddl = "CREATE TABLE public.t (id integer NOT NULL, d interval day, iv interval, jv interval);";
    let src = "UPDATE t SET d = iv WHERE iv = jv;\nUPDATE t SET d = jv WHERE iv = jv;";
    let r = lower_with_ddl(src, ddl, CatalogSource::Declared);
    assert!(is_store_refusal(&r, "interval"), "{r:?}");
}

#[test]
fn a_cast_that_converts_by_value_stores_the_class() {
    // numeric(10,2) rounds 2.0 and 2.00 to 2.00, an integer column rounds both to 2, a float column
    // converts both to 2, and a numeric column stores -0 and 0 as 0.
    for col in ["n2", "i", "f"] {
        lowers(
            &format!("UPDATE t SET {col} = n WHERE n = m"),
            &format!("UPDATE t SET {col} = m WHERE n = m"),
        );
    }
    lowers(r#"UPDATE t SET n = f WHERE f = g"#, r#"UPDATE t SET n = g WHERE f = g"#);
    lowers(
        r#"INSERT INTO t (id, n2) SELECT k, n FROM u WHERE n = m"#,
        r#"INSERT INTO t (id, n2) SELECT k, m FROM u WHERE n = m"#,
    );
    // jsonb into jsonb, and text into text, are the identity.
    lowers(r#"UPDATE t SET jb = jb WHERE id = 1"#, r#"UPDATE t SET jb = jb WHERE id = 1 AND TRUE"#);
    let v = lowers(
        r#"INSERT INTO t (id, s) SELECT DISTINCT k, s FROM u"#,
        r#"INSERT INTO t (id, s) SELECT k, s FROM u GROUP BY k, s"#,
    );
    assert!(exact_terms(&v["queries"][0]).is_empty());
    // A numeric column the SET does not name is projected as it was, and stored as it was.
    let v = lowers(r#"UPDATE t SET s = 'x' WHERE n = m"#, r#"UPDATE t SET s = 'y' WHERE n = m"#);
    assert!(exact_terms(&v["queries"][0]).is_empty());
}

#[test]
fn a_value_fixed_by_its_spelling_is_stored_through_it() {
    // `SET s = 2.0` stores '2.0' and `SET s = 2.00` stores '2.00': two terms, which no prover equates.
    let v = lowers(r#"UPDATE t SET s = 2.0 WHERE id = 1"#, r#"UPDATE t SET s = 2.00 WHERE id = 1"#);
    let (e0, e1) = (exact_terms(&v["queries"][0]), exact_terms(&v["queries"][1]));
    assert_eq!((e0.len(), e1.len()), (1, 1), "{e0:?} {e1:?}");
    assert_ne!(e0, e1);
    assert_eq!(e0[0]["operator"], "q_exact_real");
    let v = lowers(r#"INSERT INTO t (id, s) VALUES (1, 2.0)"#, r#"INSERT INTO t (id, s) VALUES (1, 2.00)"#);
    let (e0, e1) = (exact_terms(&v["queries"][0]), exact_terms(&v["queries"][1]));
    assert_eq!((e0.len(), e1.len()), (1, 1), "{e0:?} {e1:?}");
    assert_ne!(e0, e1);
    // One spelling on both sides is one term: the integer column is an input to it.
    let v = lowers(
        r#"INSERT INTO t (id, s) SELECT k, k * 1.5 FROM u"#,
        r#"INSERT INTO t (id, s) SELECT k, k * 1.5 FROM u WHERE TRUE"#,
    );
    let (e0, e1) = (exact_terms(&v["queries"][0]), exact_terms(&v["queries"][1]));
    assert_eq!(e0.len(), 1, "{e0:?}");
    assert_eq!(e0, e1);
    // An interval literal stored in an `interval day` column.
    let v = lowers(
        r#"UPDATE t SET d = INTERVAL '24 hours' WHERE id = 1"#,
        r#"UPDATE t SET d = INTERVAL '1 day' WHERE id = 1"#,
    );
    assert_ne!(exact_terms(&v["queries"][0]), exact_terms(&v["queries"][1]));
}

#[test]
fn one_plan_does_not_lift_the_refusal() {
    // A DISTINCT keeps whichever of 2.0 and 2.00 reaches it first, and the dropped ORDER BY decides
    // which: over u = {(1, 2.0), (2, 2.00)}, A inserts '2.0' and B '2.00', though both lower to one
    // plan once the dead ordering is stripped.
    refused(
        r#"INSERT INTO t (s) SELECT DISTINCT n FROM (SELECT n FROM u ORDER BY k) x"#,
        r#"INSERT INTO t (s) SELECT DISTINCT n FROM (SELECT n FROM u ORDER BY k DESC) x"#,
        "numeric",
    );
}
