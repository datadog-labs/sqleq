// Unless explicitly stated otherwise all files in this repository are licensed under the
// Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/).
// Copyright 2026-Present Datadog, Inc.

//! Lowerings that used to make two different queries look alike to a prover.
//!
//! Each "not identical" or "refused" test is a pair that is **not** equivalent in Postgres and used
//! to lower to byte-identical IR, or to IR a prover would read the wrong way. Each has a control
//! beside it that keeps the equivalent shape lowering as before. They do not run a prover; they pin
//! the lowering.

use serde_json::Value;
use sqleq_frontend::{lower_with, CatalogSource, FrontendError};

const T: &str = r#"create table "t" ("id" INTEGER, "a" INTEGER, "s" VARCHAR, "k" VARCHAR, "ts" TIMESTAMP, "c" VARBINARY, unique ("id"));"#;

fn lower(q0: &str, q1: &str, src: CatalogSource) -> Value {
    lower_with(&format!("{T}\n{q0};\n{q1};"), src).unwrap_or_else(|e| panic!("expected Ok, got {e}"))
}

/// Whether the pair lowers to byte-identical queries in the given catalog mode.
fn identical_in(q0: &str, q1: &str, src: CatalogSource) -> bool {
    let v = lower(q0, q1, src);
    v["queries"][0] == v["queries"][1]
}

/// The same, in the corpus runs' mode.
fn identical(q0: &str, q1: &str) -> bool {
    identical_in(q0, q1, CatalogSource::InferredSeeded)
}

fn operators(v: &Value, out: &mut Vec<String>) {
    match v {
        Value::Object(m) => {
            if let Some(Value::String(o)) = m.get("operator") {
                out.push(o.clone());
            }
            m.values().for_each(|x| operators(x, out));
        }
        Value::Array(a) => a.iter().for_each(|x| operators(x, out)),
        _ => {}
    }
}

fn ops_of(q0: &str, q1: &str) -> Vec<String> {
    let mut out = Vec::new();
    operators(&lower(q0, q1, CatalogSource::InferredSeeded), &mut out);
    out
}

fn refused(q0: &str, q1: &str, needle: &str) {
    match lower_with(&format!("{T}\n{q0};\n{q1};"), CatalogSource::InferredSeeded) {
        Err(FrontendError::Unsupported(m)) => assert!(m.contains(needle), "refused for {m:?}"),
        Err(e) => panic!("expected an Unsupported refusal mentioning {needle:?}, got {e}"),
        Ok(_) => panic!("expected a refusal mentioning {needle:?}, but it lowered"),
    }
}

#[test]
fn a_with_binding_named_like_the_dml_target_is_refused() {
    // The target names the table, so the first statement empties `t`.
    refused(
        "WITH t AS (SELECT * FROM t WHERE a = 1) DELETE FROM t",
        "DELETE FROM t WHERE a = 1",
        "WITH binding named like the DML target",
    );
    refused(
        "WITH t AS (SELECT id, 0 AS a, s, k, ts, c FROM t) UPDATE t SET a = a",
        "UPDATE t SET a = 0",
        "WITH binding named like the DML target",
    );
}

#[test]
fn a_typmod_on_a_parameter_cast_is_kept() {
    // `varchar(n)` truncates and `timestamp(p)` rounds, so the typmod is part of what is computed.
    assert!(!identical("SELECT id FROM t WHERE k = $1::varchar(2)", "SELECT id FROM t WHERE k = $1::varchar(3)"));
    assert!(!identical("SELECT id FROM t WHERE k = $1::text", "SELECT id FROM t WHERE k = $1::varchar(255)"));
    assert!(!identical("SELECT id FROM t WHERE ts = $1::timestamp(0)", "SELECT id FROM t WHERE ts = $1::timestamp"));
    // An unqualified cast over a parameter is still just its type.
    assert!(identical("SELECT id FROM t WHERE k = $1::varchar", "SELECT id FROM t WHERE k = $1"));
}

#[test]
fn a_typmod_on_a_literal_cast_is_kept() {
    assert!(!identical("SELECT id FROM t WHERE s = 'abc'::varchar(2)", "SELECT id FROM t WHERE s = 'abc'::varchar(3)"));
    assert!(!identical(
        "SELECT id FROM t WHERE ts = '2020-01-01 01:00:00.5'::timestamp(0)",
        "SELECT id FROM t WHERE ts = '2020-01-01 01:00:00.5'::timestamp"
    ));
    // One spelling on both sides is still one function.
    assert!(identical("SELECT id FROM t WHERE s = 'abc'::varchar(2)", "SELECT id FROM t WHERE s = 'abc'::VARCHAR(2)"));
}

#[test]
fn a_typmod_on_a_column_cast_is_kept_in_declared_mode() {
    let declared = CatalogSource::Declared;
    assert!(!identical_in("SELECT k::varchar(2) FROM t", "SELECT k::varchar(3) FROM t", declared));
    assert!(identical_in("SELECT k::varchar(2) FROM t", "SELECT k::VARCHAR(2) FROM t", declared));
}

#[test]
fn a_cast_to_an_array_type_is_not_the_identity() {
    // Every array is one opaque IR type, and a cast between equal IR types reads as the identity;
    // but `ys::int[]` parses each element of a `text[]`.
    let ddl = "CREATE TABLE u (id integer, s text, xs int[], ys text[]);";
    let q = "SELECT ys::int[], xs::text[], s::varchar(1), ys::varchar(1)[] FROM u";
    let v = lower_with(&format!("{ddl}\n{q};\n{q};"), CatalogSource::Declared)
        .unwrap_or_else(|e| panic!("expected Ok, got {e}"));
    let mut ops = Vec::new();
    operators(&v["queries"][0], &mut ops);
    assert!(!ops.iter().any(|o| o == "CAST"), "{ops:?}");
    // A scalar typmod and the same one on an array are two functions.
    for name in ["q_cast_int_array", "q_cast_text_array", "q_cast_varchar_1", "q_cast_varchar_1_array"] {
        assert!(ops.iter().any(|o| o == name), "{name} in {ops:?}");
    }
}

#[test]
fn two_columns_whose_names_differ_only_in_case_are_refused() {
    // Quoted names keep their case, so `"S"` and `"s"` are two columns; the catalog folds both.
    let ddl = r#"create table "u" ("id" INTEGER, "s" VARCHAR, "S" VARCHAR);"#;
    for src in [CatalogSource::Declared, CatalogSource::InferredSeeded] {
        match lower_with(&format!("{ddl}\nSELECT \"S\" FROM u;\nSELECT \"s\" FROM u;"), src) {
            Err(FrontendError::Unsupported(m)) => assert!(m.contains("up to case"), "{m}"),
            other => panic!("expected a refusal, got {other:?}"),
        }
    }
}

#[test]
fn a_set_returning_function_over_aggregates_is_refused() {
    refused(
        "SELECT jsonb_array_elements_text(jsonb_build_array(count(t.id), count(t.id))) FROM t",
        "SELECT DISTINCT jsonb_array_elements_text(jsonb_build_array(count(t.id), count(t.id))) FROM t",
        "set-returning function",
    );
}

#[test]
fn concatenation_over_an_opaque_operand_is_not_text_concatenation() {
    // Array `||` is not strict: `'{a}' || NULL` is `{a}`.
    let (q0, q1) = ("SELECT id FROM t WHERE (c || $1) IS NULL", "SELECT id FROM t WHERE c IS NULL OR $1 IS NULL");
    assert!(!identical(q0, q1));
    let ops = ops_of(q0, q1);
    assert!(!ops.iter().any(|o| o == "||"), "{ops:?}");
    assert!(ops.iter().any(|o| o.starts_with("q_op_concat_varbinary_")), "{ops:?}");
    // Text concatenation stays the native operator.
    assert!(ops_of("SELECT s || 'x' FROM t", "SELECT s || 'x' FROM t").iter().any(|o| o == "||"));
}

#[test]
fn a_quantified_like_pattern_is_refused() {
    // `NULL LIKE ALL('{}')` is TRUE, so the quantified form is not a strict `LIKE`.
    refused("SELECT id FROM t WHERE s LIKE ALL($1)", "SELECT id FROM t WHERE s LIKE $1", "LIKE ALL");
    refused("SELECT id FROM t WHERE s LIKE SOME($1)", "SELECT id FROM t WHERE s LIKE $1", "LIKE SOME");
    refused("SELECT id FROM t WHERE s SIMILAR TO ANY($1)", "SELECT id FROM t WHERE s LIKE $1", "SIMILAR TO ANY");
}

#[test]
fn any_over_an_array_of_opaque_elements_is_not_expanded() {
    // Over `ARRAY[c]` with `c` an array, `ANY` ranges over the leaves of the result.
    refused("SELECT id FROM t WHERE s = ANY(ARRAY[c])", "SELECT id FROM t", "with an element of opaque type");
    // Scalar elements still expand, exactly.
    assert!(identical("SELECT id FROM t WHERE k = ANY(ARRAY[$1, $2])", "SELECT id FROM t WHERE k = $1 OR k = $2"));
}

#[test]
fn an_array_column_is_opaque_in_inline_ddl() {
    let ddl = "CREATE TABLE u (id integer, xs int[], ys text[]);";
    let v = lower_with(&format!("{ddl}\nSELECT xs, ys FROM u;\nSELECT xs, ys FROM u;"), CatalogSource::Declared)
        .unwrap_or_else(|e| panic!("expected Ok, got {e}"));
    let types: Vec<&str> = v["schemas"][0]["types"].as_array().expect("types").iter().filter_map(|t| t.as_str()).collect();
    assert_eq!(types, ["INTEGER", "VARBINARY", "VARBINARY"], "{v}");
}
