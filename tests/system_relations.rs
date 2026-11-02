// Unless explicitly stated otherwise all files in this repository are licensed under the
// Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/).
// Copyright 2026-Present Datadog, Inc.

//! A relation Postgres reads from a system schema is not a table the DDL declares (#202).
//!
//! Postgres searches `pg_catalog` before the search path unless the path names it, so after
//! `CREATE TABLE pg_attribute (attname TEXT PRIMARY KEY, …)`, which puts a table in `public`, a bare
//! `FROM pg_attribute` still reads `pg_catalog.pg_attribute`, with many rows per `attname`. Lowered
//! against the declared key, `SELECT DISTINCT attname` and `SELECT attname` were one query, and both
//! provers proved them so. Each test below names such a relation one way (bare, under a system
//! schema, as a DML target, through every reader of the DDL) and expects a refusal; the controls
//! are the user's own `pg_*` tables, read through their schema, which still lower.

use sqleq_frontend::{lower_with, lower_with_ddl, CatalogSource, FrontendError};

const MODES: [CatalogSource; 3] = [CatalogSource::Declared, CatalogSource::InferredSeeded, CatalogSource::Inferred];

/// `ddl` and the two statements, lowered under `mode` from a pair file.
fn lower(ddl: &str, a: &str, b: &str, mode: CatalogSource) -> sqleq_frontend::Result<serde_json::Value> {
    lower_with(&format!("{ddl}\n{a};\n{b};"), mode)
}

fn assert_system_refusal(r: sqleq_frontend::Result<serde_json::Value>, what: &str) {
    match r {
        Err(FrontendError::Unsupported(m)) if m.contains("system relation") => {}
        other => panic!("{what}: expected a system-relation refusal, got {other:?}"),
    }
}

/// The issue's pair, through both readers of the DDL and every catalog mode.
#[test]
fn a_bare_catalog_name_is_refused_whatever_the_ddl_declares() {
    let ddl = r#"create table "pg_attribute" ("attname" TEXT PRIMARY KEY, "attnum" INTEGER);"#;
    let (a, b) = (r#"SELECT DISTINCT "attname" FROM "pg_attribute""#, r#"SELECT "attname" FROM "pg_attribute""#);
    for mode in MODES {
        assert_system_refusal(lower(ddl, a, b, mode), &format!("pair file, {mode:?}"));
        assert_system_refusal(lower_with_ddl(&format!("{a};\n{b};"), ddl, mode), &format!("raw DDL, {mode:?}"));
        // And with no DDL at all: Postgres runs the query, so this is not an unknown table.
        assert_system_refusal(lower("", a, b, mode), &format!("no DDL, {mode:?}"));
    }
    // Unquoted, and joined beside a user table.
    let t = r#"create table "t" ("a" INTEGER PRIMARY KEY);"#;
    assert_system_refusal(
        lower(t, r#"SELECT "a" FROM "t", PG_CLASS"#, r#"SELECT "a" FROM "t""#, CatalogSource::Declared),
        "unquoted, in a join",
    );
}

/// A system schema is the server's even where the DDL claims a table in it, which Postgres would
/// refuse to create; and every `pg_` schema counts, not only those a list names.
#[test]
fn a_name_under_a_system_schema_is_refused() {
    let cases = [
        (r#"create table "pg_catalog"."pg_attribute" ("attname" TEXT PRIMARY KEY);"#, r#""pg_catalog"."pg_attribute""#),
        (r#"create table "information_schema"."tables" ("table_name" TEXT PRIMARY KEY);"#, r#""information_schema"."tables""#),
        // Stripped to the declared `t` before: `pg_toast` was not on the list of system schemas.
        (r#"create table "t" ("a" INTEGER PRIMARY KEY);"#, r#""pg_toast"."t""#),
        (r#"create table "t" ("a" INTEGER PRIMARY KEY);"#, r#""pg_temp_3"."t""#),
        // A bare `t` resolves to the one table of its name the DDL declares, here under `pg_catalog`.
        (r#"create table "pg_catalog"."t" ("a" INTEGER PRIMARY KEY);"#, r#""t""#),
    ];
    for (ddl, rel) in cases {
        let col = if ddl.contains("attname") { "attname" } else if ddl.contains("table_name") { "table_name" } else { "a" };
        let (a, b) = (format!(r#"SELECT DISTINCT "{col}" FROM {rel}"#), format!(r#"SELECT "{col}" FROM {rel}"#));
        for mode in [CatalogSource::Declared, CatalogSource::InferredSeeded] {
            assert_system_refusal(lower(ddl, &a, &b, mode), &format!("{rel} over {ddl}, {mode:?}"));
        }
    }
}

/// A DML target: `UPDATE` and `DELETE` read it as a scan, and an `INSERT`'s target is replaced by its
/// source, so it is checked where the reduction resolves it.
#[test]
fn a_system_relation_target_is_refused() {
    let ddl = r#"create table "pg_attribute" ("attname" TEXT PRIMARY KEY, "attnum" INTEGER);"#;
    let qualified = r#"create table "pg_catalog"."pg_attribute" ("attname" TEXT PRIMARY KEY, "attnum" INTEGER);"#;
    let under_catalog = r#"create table "pg_catalog"."t" ("a" INTEGER PRIMARY KEY, "b" INTEGER);"#;
    for (ddl, a, b) in [
        (ddl, r#"INSERT INTO "pg_attribute" ("attname", "attnum") VALUES ('a', 1)"#,
            r#"INSERT INTO "pg_attribute" ("attname", "attnum") VALUES ('a', 2 - 1)"#),
        (qualified, r#"INSERT INTO "pg_catalog"."pg_attribute" ("attname", "attnum") VALUES ('a', 1)"#,
            r#"INSERT INTO "pg_catalog"."pg_attribute" ("attname", "attnum") VALUES ('a', 2 - 1)"#),
        (under_catalog, r#"INSERT INTO "t" ("a", "b") VALUES (1, 1)"#, r#"INSERT INTO "t" ("a", "b") VALUES (1, 2 - 1)"#),
        (ddl, r#"UPDATE "pg_attribute" SET "attnum" = 1"#, r#"UPDATE "pg_attribute" SET "attnum" = 2 - 1"#),
        (ddl, r#"DELETE FROM "pg_attribute" WHERE "attnum" = 1"#, r#"DELETE FROM "pg_attribute" WHERE 1 = "attnum""#),
    ] {
        assert_system_refusal(lower(ddl, a, b, CatalogSource::Declared), a);
    }
}

/// The controls: a `pg_*` table read through the user's schema is that table, and a `WITH` binding
/// shadows the catalog in Postgres as it does here.
#[test]
fn a_users_pg_named_table_and_a_binding_still_lower() {
    let public = r#"create table "public"."pg_attribute" ("attname" TEXT PRIMARY KEY, "attnum" INTEGER);"#;
    let s = r#"create table "s"."pg_x" ("a" INTEGER PRIMARY KEY);"#;
    let t = r#"create table "t" ("a" INTEGER PRIMARY KEY);"#;
    for (ddl, a, b) in [
        (public, r#"SELECT DISTINCT "attname" FROM "public"."pg_attribute""#, r#"SELECT "attname" FROM "public"."pg_attribute""#),
        (s, r#"SELECT DISTINCT "a" FROM "s"."pg_x""#, r#"SELECT "a" FROM "s"."pg_x""#),
        (t, r#"WITH "pg_class" AS (SELECT "a" FROM "t") SELECT "a" FROM "pg_class""#, r#"SELECT "a" FROM "t""#),
        (public, r#"INSERT INTO "public"."pg_attribute" ("attname", "attnum") VALUES ('a', 1)"#,
            r#"INSERT INTO "public"."pg_attribute" ("attname", "attnum") VALUES ('a', 2 - 1)"#),
    ] {
        for mode in [CatalogSource::Declared, CatalogSource::InferredSeeded] {
            lower(ddl, a, b, mode).unwrap_or_else(|e| panic!("{a}, {mode:?}: {e}"));
        }
    }
}
