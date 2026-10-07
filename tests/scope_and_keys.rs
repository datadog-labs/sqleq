// Unless explicitly stated otherwise all files in this repository are licensed under the
// Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/).
// Copyright 2026-Present Datadog, Inc.

//! Two rules of the catalog and of name resolution:
//!
//! * a quoted name keeps its case, for a column and for a table alias alike, so `"A"` is not `A`
//!   (issue #57);
//! * a `DEFERRABLE` primary key or unique constraint is not a key, since Postgres lets it be violated
//!   until the transaction commits (issue #95).
//!
//! Each "apart" or "refused" test is a pair that is **not** equivalent in Postgres (its witness is
//! in the doc comment, checked on Postgres 17) and that used to lower to identical IR, or to IR that
//! read a name from the wrong relation. Controls beside them keep the spellings that are one query
//! lowering alike. They do not run a prover; they pin the lowering.

use serde_json::Value;
use sqleq_frontend::{lower_with, lower_with_ddl, CatalogSource, FrontendError};

const MODES: [CatalogSource; 2] = [CatalogSource::Declared, CatalogSource::InferredSeeded];

fn pair(ddl: &str, q0: &str, q1: &str) -> String {
    format!("{ddl}\n{q0};\n{q1};")
}

/// Whether the pair lowers to byte-identical queries, in both catalog modes that read the DDL. A
/// refusal in either is a test bug, so it panics.
fn identical(ddl: &str, q0: &str, q1: &str) -> bool {
    let mut seen = Vec::new();
    for src in MODES {
        let v = lower_with(&pair(ddl, q0, q1), src);
        let v = v.unwrap_or_else(|e| panic!("{src:?}: expected Ok, got {e}"));
        seen.push(v["queries"][0] == v["queries"][1]);
    }
    assert_eq!(seen[0], seen[1], "the two catalog modes disagree");
    seen[0]
}

/// Whether the pair lowers to byte-identical queries under the declared catalog alone. For a pair
/// that type inference refuses, as it refuses a bare name two tables declare up to case.
fn identical_declared(ddl: &str, q0: &str, q1: &str) -> bool {
    let v = lower_with(&pair(ddl, q0, q1), CatalogSource::Declared);
    let v = v.unwrap_or_else(|e| panic!("expected Ok, got {e}"));
    v["queries"][0] == v["queries"][1]
}

/// Whether the pair is kept apart in both modes: refused, or lowered to two different queries.
fn apart(ddl: &str, q0: &str, q1: &str) -> bool {
    MODES.iter().all(|&src| match lower_with(&pair(ddl, q0, q1), src) {
        Ok(v) => v["queries"][0] != v["queries"][1],
        Err(_) => true,
    })
}

/// Assert the pair is refused in both modes, as unsupported or as a schema error as `kind` says,
/// with `needle` in the reason.
fn refused(ddl: &str, q0: &str, q1: &str, kind: &str, needle: &str) {
    for src in MODES {
        match (kind, lower_with(&pair(ddl, q0, q1), src)) {
            ("unsupported", Err(FrontendError::Unsupported(m))) | ("schema", Err(FrontendError::Schema(m))) => {
                assert!(m.contains(needle), "{src:?}: refused for {m:?}, not {needle:?}")
            }
            (_, other) => panic!("{src:?}: expected a {kind} refusal mentioning {needle:?}, got {other:?}"),
        }
    }
}

// --- #57: a quoted name keeps its case -------------------------------------------------------

/// `m` has a quoted column `"A"`, which is not `a`; `t` has `a`; `u` is a second table with `a`.
const QUOTED: &str = r#"create table "t" ("id" INTEGER, "a" INTEGER);
create table "u" ("id" INTEGER, "a" INTEGER);
create table "m" ("id" INTEGER, "A" INTEGER);"#;

/// `m = {(1, 10)}`, `t = {(1, 1)}`: the first reads `m."A"` and returns 10, the second folds `A` to
/// `a`, which only `t` has, and returns 1. The catalog used to fold `"A"` to `a` as well. (Type
/// inference still attributes a bare name by its lower-cased spelling, so under the inferred-seeded
/// catalog it refuses both as a name `m` and `t` declare.)
#[test]
fn a_quoted_column_is_not_its_folded_name() {
    assert!(apart(QUOTED, r#"SELECT "A" FROM "m", "t""#, r#"SELECT A FROM "m", "t""#));
    assert!(!identical_declared(QUOTED, r#"SELECT "A" FROM "m", "t""#, r#"SELECT A FROM "m", "t""#));
    assert!(identical_declared(QUOTED, r#"SELECT "A" FROM "m", "t""#, r#"SELECT "m"."A" FROM "m", "t""#));
    assert!(identical_declared(QUOTED, r#"SELECT A FROM "m", "t""#, r#"SELECT "t"."a" FROM "m", "t""#));
    assert!(identical(QUOTED, r#"SELECT "A" FROM "m""#, r#"SELECT "m"."A" FROM "m""#));
    // A query that spells a quoted column unquoted reads a column that does not exist.
    refused(QUOTED, r#"SELECT A FROM "m""#, r#"SELECT "A" FROM "m""#, "schema", "unresolved column a");
}

/// `t = {(1, 1)}`, `u = {(1, 5)}`: the first reads `"X"`, which is `t`, and returns 1; the second
/// reads `x`, which is `u`, and returns 5. Both aliases used to be stored as `x`.
#[test]
fn a_quoted_table_alias_is_not_its_folded_name() {
    let from = r#"FROM "t" AS "X", "u" AS x"#;
    let (upper, lower) = (format!(r#"SELECT "X".a {from}"#), format!("SELECT x.a {from}"));
    assert!(apart(QUOTED, &upper, &lower));
    assert!(!identical(QUOTED, &upper, &lower));
    assert!(identical(QUOTED, &format!("SELECT X.a {from}"), &format!(r#"SELECT "x".a {from}"#)));
    // A table without an alias is referred to by its name as the query spells it.
    assert!(identical(QUOTED, r#"SELECT M."A" FROM "m""#, r#"SELECT "m"."A" FROM m"#));
}

/// Controls for a mixed-case schema of the kind ORMs write: references spelled as declared lower,
/// and an `UPDATE` or `INSERT` naming such a column still reduces.
#[test]
fn a_mixed_case_schema_still_lowers() {
    let ddl = r#"create table "Users" ("id" INTEGER PRIMARY KEY, "createdAt" INTEGER);"#;
    assert!(identical(
        ddl,
        r#"SELECT "Users"."createdAt" FROM "Users" WHERE "createdAt" > 1"#,
        r#"SELECT "u"."createdAt" FROM "Users" AS "u" WHERE "u"."createdAt" > 1"#,
    ));
    assert!(identical(
        ddl,
        r#"UPDATE "Users" SET "createdAt" = 1 WHERE "createdAt" > 2"#,
        r#"UPDATE "Users" SET "createdAt" = 1 WHERE "Users"."createdAt" > 2"#,
    ));
    for src in MODES {
        let insert = |w: &str| {
            format!(r#"INSERT INTO "Users" ("id", "createdAt") SELECT "id", "createdAt" FROM "Users" WHERE {w}"#)
        };
        lower_with(&pair(ddl, &insert(r#""id" = 1"#), &insert(r#"1 = "id""#)), src)
            .unwrap_or_else(|e| panic!("{src:?}: expected Ok, got {e}"));
    }
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
