// Unless explicitly stated otherwise all files in this repository are licensed under the
// Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/).
// Copyright 2026-Present Datadog, Inc.

//! A pair file's DDL and raw DDL are read into a catalog by one builder (issue #93).
//!
//! The two inputs used to have a builder each, and they disagreed: raw DDL keyed a table on its bare
//! name, so two schemas' `t` collided and the whole pair was refused; a pair file kept a table with
//! no columns, counted one key spelled twice as two, and kept a domain's name as a column's declared
//! type. Each test below reads one DDL both ways and expects one answer. They do not run a prover.

use serde_json::Value;
use sqleq_frontend::{lower_with, lower_with_ddl, CatalogSource, FrontendError, Result};

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

fn refused(ddl: &str, q0: &str, q1: &str, needle: &str) {
    for r in both(ddl, q0, q1) {
        match r {
            Err(FrontendError::Unsupported(m) | FrontendError::Schema(m)) => {
                assert!(m.contains(needle), "refused for {m:?}, expected {needle:?}")
            }
            Err(e) => panic!("expected a refusal mentioning {needle:?}, got {e}"),
            Ok(_) => panic!("expected a refusal mentioning {needle:?}, but it lowered\n{ddl}\n{q0}\n{q1}"),
        }
    }
}

/// The schema names a lowered pair carries.
fn names(v: &Value) -> Vec<&str> {
    v["schemas"].as_array().unwrap().iter().map(|t| t["name"].as_str().unwrap()).collect()
}

#[test]
fn a_table_is_keyed_on_its_declared_name_in_both_inputs() {
    let ddl = "CREATE TABLE public.orders (id integer PRIMARY KEY, total integer);";
    for v in lowered(ddl, "SELECT id FROM orders WHERE total > 1", "SELECT id FROM orders WHERE 1 < total") {
        assert_eq!(names(&v), ["public.orders"]);
    }
    let ddl = "CREATE TABLE s.t (id integer PRIMARY KEY, a integer);";
    for v in lowered(ddl, "SELECT id FROM s.t WHERE a > 1", "SELECT id FROM s.t WHERE 1 < a") {
        assert_eq!(names(&v), ["s.t"]);
    }
}

#[test]
fn two_schemas_declaring_one_name_are_two_tables_in_both_inputs() {
    let ddl = "CREATE TABLE a.t (x integer); CREATE TABLE b.t (x integer); CREATE TABLE u (y integer);";
    // A pair that reads neither: raw DDL used to refuse it for the two `t`s colliding.
    lowered(ddl, "SELECT y FROM u WHERE y > 1", "SELECT y FROM u WHERE 1 < y");
    // A pair that names each: two scans.
    for v in lowered(ddl, "SELECT x FROM a.t", "SELECT x FROM b.t") {
        assert_ne!(v["queries"][0], v["queries"][1]);
    }
    // A bare name both declare is neither.
    refused(ddl, "SELECT x FROM t", "SELECT x FROM t WHERE x > 1", "unknown table t");
}

#[test]
fn a_table_with_no_columns_is_not_read_in_either_input() {
    let ddl = "CREATE TABLE u (a integer); CREATE TABLE c AS SELECT a FROM u;";
    refused(ddl, "SELECT * FROM c", "SELECT DISTINCT * FROM c", "unknown table c");
    lowered(ddl, "SELECT a FROM u WHERE a > 1", "SELECT a FROM u WHERE 1 < a");
}

#[test]
fn one_key_spelled_twice_is_one_key_in_both_inputs() {
    let ddl = "CREATE TABLE t (id integer NOT NULL UNIQUE, a integer, UNIQUE (id));";
    for v in lowered(ddl, "SELECT id FROM t", "SELECT id FROM t WHERE a > 1") {
        assert_eq!(v["schemas"][0]["key"], serde_json::json!([[0]]));
    }
}

#[test]
fn a_domain_column_is_assigned_as_its_base_type_in_both_inputs() {
    // An assignment to `interval day` truncates to the day, so storing a value `=` calls equal to
    // `1 day` (`24 hours`) is not storing `1 day`; through a domain as much as directly.
    let ddl = "CREATE DOMAIN d AS interval day; CREATE TABLE t (id integer PRIMARY KEY, x d, y interval);";
    refused(
        ddl,
        "UPDATE t SET x = y WHERE y = INTERVAL '1 day'",
        "UPDATE t SET x = INTERVAL '1 day' WHERE y = INTERVAL '1 day'",
        "INTERVAL DAY",
    );
}
