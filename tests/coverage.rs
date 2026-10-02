// Unless explicitly stated otherwise all files in this repository are licensed under the
// Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/).
// Copyright 2026-Present Datadog, Inc.

//! Expressions the frontend lowers by naming what they compute.
//!
//! Each test pairs the shapes that must now lower alike with the ones that must not, and with the
//! variants that must still be refused. They do not run a prover; they pin the lowering.

use serde_json::Value;
use sqleq_frontend::{lower_with, CatalogSource, FrontendError};

const T: &str = r#"create table "t" ("id" INTEGER, "a" INTEGER, "s" VARCHAR, "k" VARCHAR, "ts" TIMESTAMP, "d" DATE, "n" REAL, unique ("id"));"#;
const U: &str = "CREATE TABLE u (id integer, s text, xs int[], ys text[], m int[][], unique (id));";

const SEEDED: CatalogSource = CatalogSource::InferredSeeded;
const DECLARED: CatalogSource = CatalogSource::Declared;

fn lower_on(ddl: &str, q0: &str, q1: &str, src: CatalogSource) -> Value {
    lower_with(&format!("{ddl}\n{q0};\n{q1};"), src).unwrap_or_else(|e| panic!("expected Ok, got {e}"))
}

fn same_on(ddl: &str, q0: &str, q1: &str, src: CatalogSource) -> bool {
    let v = lower_on(ddl, q0, q1, src);
    v["queries"][0] == v["queries"][1]
}

fn same(q0: &str, q1: &str, src: CatalogSource) -> bool {
    same_on(T, q0, q1, src)
}

fn refused_on(ddl: &str, q: &str, src: CatalogSource, needle: &str) {
    match lower_with(&format!("{ddl}\n{q};\n{q};"), src) {
        Err(FrontendError::Unsupported(m)) => assert!(m.contains(needle), "refused for {m:?}"),
        Err(e) => panic!("expected an Unsupported refusal mentioning {needle:?}, got {e}"),
        Ok(_) => panic!("expected a refusal mentioning {needle:?}, but it lowered"),
    }
}

/// Every `operator` in the first query, in tree order.
fn operators(ddl: &str, q: &str, src: CatalogSource) -> Vec<String> {
    fn walk(v: &Value, out: &mut Vec<String>) {
        match v {
            Value::Object(m) => {
                if let Some(Value::String(o)) = m.get("operator") {
                    out.push(o.clone());
                }
                m.values().for_each(|x| walk(x, out));
            }
            Value::Array(a) => a.iter().for_each(|x| walk(x, out)),
            _ => {}
        }
    }
    let mut out = Vec::new();
    walk(&lower_on(ddl, q, q, src)["queries"][0], &mut out);
    out
}

#[test]
fn a_typed_literal_lowers_as_the_cast_it_spells() {
    for src in [SEEDED, DECLARED] {
        assert!(same(
            "SELECT id FROM t WHERE ts > now() - INTERVAL '1 day'",
            "SELECT id FROM t WHERE ts > now() - '1 day'::interval",
            src
        ));
        assert!(same("SELECT id FROM t WHERE d = DATE '2020-01-01'", "SELECT id FROM t WHERE d = '2020-01-01'::date", src));
        assert!(!same("SELECT id FROM t WHERE d = DATE '2020-01-01'", "SELECT id FROM t WHERE d = DATE '2020-01-02'", src));
    }
    // A captured statement's `DATE $1` is the literal with its constant taken out.
    assert!(same("SELECT id FROM t WHERE d = DATE $1", "SELECT id FROM t WHERE d = $1::date", SEEDED));
    assert!(same("SELECT id FROM t WHERE ts > $1 - INTERVAL $2", "SELECT id FROM t WHERE ts > $1 - $2::interval", SEEDED));
    // The typmod stays on the cast.
    assert!(same("SELECT id FROM t WHERE ts = TIMESTAMP(0) $1", "SELECT id FROM t WHERE ts = $1::timestamp(0)", SEEDED));
    assert!(!same("SELECT id FROM t WHERE ts = TIMESTAMP(0) $1", "SELECT id FROM t WHERE ts = TIMESTAMP $1", SEEDED));
    assert!(!same(
        "SELECT id FROM t WHERE ts = TIMESTAMP(0) '2020-01-01 01:00:00.5'",
        "SELECT id FROM t WHERE ts = TIMESTAMP '2020-01-01 01:00:00.5'",
        SEEDED
    ));
}

#[test]
fn an_interval_field_qualifier_is_refused() {
    // `INTERVAL '1'` is one second and `INTERVAL '1' DAY` one day.
    refused_on(T, "SELECT id FROM t WHERE ts > $1 - INTERVAL $2 DAY", SEEDED, "Interval");
    refused_on(T, "SELECT id FROM t WHERE ts > now() - INTERVAL '1' DAY", SEEDED, "Interval");
}

#[test]
fn ceil_and_floor_are_calls() {
    for src in [SEEDED, DECLARED] {
        assert!(same("SELECT CEIL(n) FROM t", "SELECT ceiling(n) FROM t", src));
        assert!(same("SELECT FLOOR(n) FROM t", "SELECT floor(n) FROM t", src));
        assert!(!same("SELECT ceil(n) FROM t", "SELECT floor(n) FROM t", src));
        // Not Postgres.
        refused_on(T, "SELECT CEIL(n, 2) FROM t", src, "Ceil");
    }
}

#[test]
fn a_shared_conversion_reads_names_as_postgres_does() {
    // Unquoted names fold, so the two casts are one function and the pair cancels.
    assert!(same("SELECT sum(a)::varchar(2) FROM t", "SELECT SUM(A)::varchar(2) FROM t", SEEDED));
    // Literals do not, and different operands or targets stay apart.
    assert!(!same("SELECT (s || 'Ab')::varchar(2) FROM t", "SELECT (s || 'ab')::varchar(2) FROM t", SEEDED));
    assert!(!same("SELECT max(s)::int FROM t", "SELECT max(k)::int FROM t", SEEDED));
    assert!(!same("SELECT sum(a)::varchar(2) FROM t", "SELECT sum(a)::varchar(3) FROM t", SEEDED));
}

#[test]
fn a_parameter_in_a_row_in_list_is_one_record_comparison() {
    // A composite value compares under record semantics, where two NULL fields are equal, so it is
    // never expanded into per-field comparisons.
    let ops = operators(T, "SELECT id FROM t WHERE (a, s) IN ($1, $2)", SEEDED);
    assert_eq!(ops.iter().filter(|o| *o == "q_row_eq_2").count(), 2, "{ops:?}");
    assert!(ops.iter().any(|o| o == "OR") && !ops.iter().any(|o| o == "="), "{ops:?}");
    let ops = operators(T, "SELECT id FROM t WHERE (a, s) NOT IN ($1, $2)", SEEDED);
    assert_eq!(ops.iter().filter(|o| *o == "NOT").count(), 2, "{ops:?}");
    // A row constructor beside it is still compared field by field.
    let ops = operators(T, "SELECT id FROM t WHERE (a, s) IN ($1, (1, 'x'))", SEEDED);
    assert!(ops.iter().any(|o| o == "q_row_eq_2") && ops.iter().any(|o| o == "="), "{ops:?}");
    // Anything else standing for a row is refused.
    refused_on(T, "SELECT id FROM t WHERE (a, s) IN (k)", SEEDED, "neither a row nor a parameter");
}

#[test]
fn a_subscript_chain_is_one_call() {
    for src in [SEEDED, DECLARED] {
        assert!(same_on(U, "SELECT u.xs[1] FROM u", "SELECT xs[1] FROM u", src));
        assert!(!same_on(U, "SELECT xs[1] FROM u", "SELECT xs[2] FROM u", src));
        // `m[1]` has too few subscripts for a two-dimensional array, so it is NULL.
        assert!(!same_on(U, "SELECT m[1][2] FROM u", "SELECT (m[1])[2] FROM u", src));
        assert!(operators(U, "SELECT m[1][2] FROM u", src).iter().any(|o| o == "q_subscript_varbinary_integer_integer"));
        refused_on(U, "SELECT xs[1:2] FROM u", src, "array slice");
    }
}

#[test]
fn an_array_constructor_is_an_opaque_value() {
    for src in [SEEDED, DECLARED] {
        let ops = operators(U, "SELECT id FROM u WHERE xs = ARRAY[id, 2]", src);
        assert!(ops.iter().any(|o| o == "q_array_integer"), "{ops:?}");
        assert!(!same_on(U, "SELECT ARRAY[id, 2] FROM u", "SELECT ARRAY[2, id] FROM u", src));
        assert!(!same_on(U, "SELECT ARRAY[s] FROM u", "SELECT ARRAY[id] FROM u", src));
        // `ARRAY[NULL]` is a one-element array, so the constructor is not strict, and over aggregates
        // it is a post-aggregation value like any other.
        assert!(operators(U, "SELECT ARRAY[count(*), max(id)] FROM u", src).iter().any(|o| o == "q_array_integer"));
        refused_on(U, "SELECT id FROM u WHERE xs = ARRAY[]", src, "empty ARRAY[]");
    }
    // Under `= ANY` it is still expanded, exactly.
    assert!(same_on(U, "SELECT id FROM u WHERE id = ANY(ARRAY[1, 2])", "SELECT id FROM u WHERE id = 1 OR id = 2", SEEDED));
}

const V: &str = r#"create table "t" ("id" INTEGER, "a" INTEGER, "s" VARCHAR, unique ("id")); create table "u" ("uid" INTEGER, "ua" INTEGER, "s" VARCHAR, unique ("uid"));"#;

#[test]
fn a_join_delete_lowers_like_its_semi_join() {
    for src in [SEEDED, DECLARED] {
        assert!(same_on(
            V,
            "DELETE FROM t USING u WHERE t.a = u.ua RETURNING t.*",
            "DELETE FROM t WHERE EXISTS (SELECT 1 FROM u WHERE t.a = u.ua) RETURNING *",
            src
        ));
        // What a returned `u` column holds depends on which `u` row the join matched.
        refused_on(V, "DELETE FROM t USING u WHERE t.a = u.ua RETURNING u.ua", src, "RETURNING item");
        refused_on(V, "DELETE FROM t USING u WHERE t.a = u.ua RETURNING *", src, "RETURNING item");
        // `s` is a column of both, so the bare name may be `u`'s.
        refused_on(V, "DELETE FROM t USING u WHERE t.a = u.ua RETURNING s", src, "RETURNING item");
    }
}

#[test]
fn a_join_update_that_only_filters_lowers_like_its_semi_join() {
    for src in [SEEDED, DECLARED] {
        assert!(same_on(
            V,
            "UPDATE t SET a = a + 1 FROM u WHERE t.id = u.uid RETURNING t.*",
            "UPDATE t SET a = a + 1 WHERE EXISTS (SELECT 1 FROM u WHERE t.id = u.uid) RETURNING *",
            src
        ));
        // When several `u` rows match, the value would come from an unspecified one.
        refused_on(V, "UPDATE t SET a = u.ua FROM u WHERE t.id = u.uid", src, "SET value");
        refused_on(V, "UPDATE t SET a = ua FROM u WHERE t.id = u.uid", src, "SET value");
        refused_on(V, "UPDATE t SET a = 0 FROM u WHERE t.id = u.uid RETURNING *", src, "RETURNING item");
        refused_on(V, "UPDATE t SET a = 0 FROM u AS t WHERE t.uid = 1", src, "named like the target");
    }
}

#[test]
fn a_join_delete_or_update_reads_through_a_parenthesized_join() {
    for src in [SEEDED, DECLARED] {
        assert!(same_on(
            V,
            "DELETE FROM t USING (u JOIN u AS w ON u.uid = w.ua) WHERE t.a = u.ua",
            "DELETE FROM t USING u JOIN u AS w ON u.uid = w.ua WHERE t.a = u.ua",
            src
        ));
        assert!(same_on(
            V,
            "UPDATE t SET a = a + 1 FROM (u JOIN u AS w ON u.uid = w.ua) WHERE t.id = w.uid",
            "UPDATE t SET a = a + 1 WHERE EXISTS (SELECT 1 FROM u JOIN u AS w ON u.uid = w.ua WHERE t.id = w.uid)",
            src
        ));
        // A table inside the parentheses is as much a FROM relation as one outside them.
        refused_on(V, "UPDATE t SET a = w.ua FROM (u JOIN u AS w ON u.uid = w.ua) WHERE t.id = u.uid", src, "SET value");
    }
}
