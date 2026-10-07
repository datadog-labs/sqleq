// Unless explicitly stated otherwise all files in this repository are licensed under the
// Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/).
// Copyright 2026-Present Datadog, Inc.

//! A rule of the catalog: a `DEFERRABLE` primary key or unique constraint is not a key, since
//! Postgres lets it be violated until the transaction commits (issue #95).
//!
//! The tests read the keys the frontend hands the provers, through both readers of a schema. They do
//! not run a prover.

use serde_json::Value;
use sqleq_frontend::{lower_with, lower_with_ddl, CatalogSource};

fn pair(ddl: &str, q0: &str, q1: &str) -> String {
    format!("{ddl}\n{q0};\n{q1};")
}

// --- #95: a deferrable key is not a key ------------------------------------------------------

/// The key column sets the frontend tells the provers for the one table in `ddl`, read once through
/// the pair-file reader and once through the raw-DDL reader, with that table's nullability.
fn keys(ddl: &str) -> Vec<(Value, Value)> {
    let q = r#"SELECT "id" FROM "t""#;
    let from_pair = lower_with(&pair(ddl, q, q), CatalogSource::Declared).expect("lowers");
    let from_ddl = lower_with_ddl(&format!("{q};\n{q};"), ddl, CatalogSource::Declared).expect("lowers");
    [from_pair, from_ddl]
        .iter()
        .map(|v| (v["schemas"][0]["key"].clone(), v["schemas"][0]["nullable"].clone()))
        .collect()
}

/// Inside a transaction, `t = {(1, 5), (1, 5)}` before commit: `SELECT DISTINCT id, a FROM t`
/// returns one row, `SELECT id, a FROM t` two. Each of these constraints allows that state, so none
/// of them is a key. The NOT NULL a primary key implies is checked at once, deferrable or not, and
/// stays.
#[test]
fn a_deferrable_key_is_not_a_key() {
    let not_null = serde_json::json!([false, true]);
    let nullable = serde_json::json!([true, true]);
    for (ddl, nulls) in [
        (r#"create table "t" ("id" INTEGER PRIMARY KEY DEFERRABLE INITIALLY DEFERRED, "a" INTEGER);"#, &not_null),
        // `INITIALLY DEFERRED` alone implies `DEFERRABLE`.
        (r#"create table "t" ("id" INTEGER PRIMARY KEY INITIALLY DEFERRED, "a" INTEGER);"#, &not_null),
        // `INITIALLY IMMEDIATE` is deferred by `SET CONSTRAINTS ... DEFERRED`.
        (r#"create table "t" ("id" INTEGER NOT NULL UNIQUE DEFERRABLE INITIALLY IMMEDIATE, "a" INTEGER);"#, &not_null),
        (r#"create table "t" ("id" INTEGER, "a" INTEGER, PRIMARY KEY ("id") DEFERRABLE);"#, &not_null),
        (r#"create table "t" ("id" INTEGER NOT NULL, "a" INTEGER, UNIQUE ("id") DEFERRABLE INITIALLY DEFERRED);"#, &not_null),
        (r#"create table "t" ("id" INTEGER, "a" INTEGER, CONSTRAINT "k" UNIQUE ("id") DEFERRABLE);"#, &nullable),
    ] {
        for (k, n) in keys(ddl) {
            assert_eq!(k, serde_json::json!([]), "{ddl}: a deferrable key was kept");
            assert_eq!(&n, nulls, "{ddl}: nullability");
        }
    }
}

/// Controls: a key that is checked at every statement is still a key, whatever it says about it.
#[test]
fn a_key_checked_at_every_statement_is_still_a_key() {
    for ddl in [
        r#"create table "t" ("id" INTEGER PRIMARY KEY, "a" INTEGER);"#,
        r#"create table "t" ("id" INTEGER PRIMARY KEY NOT DEFERRABLE, "a" INTEGER);"#,
        r#"create table "t" ("id" INTEGER PRIMARY KEY INITIALLY IMMEDIATE, "a" INTEGER);"#,
        r#"create table "t" ("id" INTEGER NOT NULL, "a" INTEGER, UNIQUE ("id") NOT DEFERRABLE INITIALLY IMMEDIATE);"#,
    ] {
        for (k, n) in keys(ddl) {
            assert_eq!(k, serde_json::json!([[0]]), "{ddl}: the key was lost");
            assert_eq!(n, serde_json::json!([false, true]), "{ddl}: nullability");
        }
    }
}
