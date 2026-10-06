// Unless explicitly stated otherwise all files in this repository are licensed under the
// Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/).
// Copyright 2026-Present Datadog, Inc.

//! The generated instance is one Postgres would accept, every table is the table Postgres would
//! resolve, and DuckDB computes Postgres's value on it (issue #64).

use sqleq_fuzz::schema::parse_schema;
use sqleq_fuzz::{test_pair, Config};

const CFG: Config = Config {
    trials: 120,
    nrows: 5,
    seed: 0,
};

fn label(a: &str, b: &str, ddl: &str) -> String {
    test_pair(a, b, ddl, CFG).label()
}

#[test]
fn a_table_level_primary_key_is_not_null() {
    let ddl = r#"create table "t" ("id" INTEGER, "a" INTEGER, PRIMARY KEY ("id"))"#;
    assert!(parse_schema(ddl)["t"].cols[0].notnull);
    assert_eq!(
        label(
            r#"SELECT "id", "a" FROM "t""#,
            r#"SELECT "id", "a" FROM "t" WHERE "id" IS NOT NULL"#,
            ddl
        ),
        "NO-COUNTEREXAMPLE"
    );
    // The parser-rejected path too: `primary` as a column name sends the table to the fallback.
    let s = parse_schema("create table q (primary boolean, id integer, PRIMARY KEY (id))");
    assert!(s["q"].cols[1].notnull, "{:?}", s["q"].cols);
}

#[test]
fn constraints_added_by_alter_table_are_enforced() {
    let pair = |ddl: &str| {
        label(
            r#"SELECT "id" FROM "t""#,
            r#"SELECT DISTINCT "id" FROM "t""#,
            ddl,
        )
    };
    for ddl in [
        r#"create table "t" ("id" INTEGER NOT NULL, "a" INTEGER); alter table "t" add primary key ("id")"#,
        r#"create table "t" ("id" INTEGER NOT NULL, "a" INTEGER); alter table only "t" add constraint "t_pk" primary key ("id")"#,
        r#"create table "t" ("id" INTEGER, "a" INTEGER); alter table "t" add unique nulls not distinct ("id")"#,
    ] {
        assert_eq!(pair(ddl), "NO-COUNTEREXAMPLE", "{ddl}");
    }
    let s = parse_schema(
        r#"create table "t" ("id" INTEGER, "a" INTEGER); alter table "t" alter column "a" set not null"#,
    );
    assert!(s["t"].cols[1].notnull);
}

#[test]
fn a_unique_index_on_an_expression_is_enforced() {
    let ddl = r#"create table "t" ("id" INTEGER NOT NULL, "c" TEXT NOT NULL, unique ("id"));
                 create unique index "t_c_lower" on "t" (lower("c"))"#;
    assert_eq!(
        label(r#"SELECT "c" FROM "t""#, r#"SELECT DISTINCT "c" FROM "t""#, ddl),
        "NO-COUNTEREXAMPLE"
    );
}

#[test]
fn nulls_not_distinct_admits_one_null() {
    for ddl in [
        r#"create table "t" ("id" INTEGER, "a" INTEGER, unique nulls not distinct ("a"))"#,
        r#"create table "t" ("id" INTEGER, "a" INTEGER unique nulls not distinct)"#,
        r#"create table "t" ("id" INTEGER, "a" INTEGER); create unique index "i" on "t" ("a") nulls not distinct"#,
    ] {
        assert_eq!(
            label(r#"SELECT "a" FROM "t""#, r#"SELECT DISTINCT "a" FROM "t""#, ddl),
            "NO-COUNTEREXAMPLE",
            "{ddl}"
        );
    }
    // A plain UNIQUE still admits many NULLs, so the same pair is refuted there.
    assert_eq!(
        label(
            r#"SELECT "a" FROM "t""#,
            r#"SELECT DISTINCT "a" FROM "t""#,
            r#"create table "t" ("id" INTEGER, "a" INTEGER, unique ("a"))"#
        ),
        "NOT-EQUIVALENT"
    );
}

#[test]
fn public_qualified_and_bare_names_are_one_table() {
    let ddl = r#"create table "t" ("id" INTEGER, "a" INTEGER, unique ("id"))"#;
    for (a, b) in [
        (
            r#"DELETE FROM "public"."t" WHERE "a" = 1"#,
            r#"DELETE FROM "t" WHERE "a" = 1"#,
        ),
        (
            r#"DELETE FROM "t" WHERE "a" = 1"#,
            r#"DELETE FROM public.t WHERE "a" = 1"#,
        ),
        (
            r#"UPDATE "public"."t" SET "a" = 0 WHERE "a" = 1"#,
            r#"UPDATE "t" SET "a" = 0 WHERE "a" = 1"#,
        ),
    ] {
        assert_eq!(label(a, b, ddl), "NO-COUNTEREXAMPLE", "{a} / {b}");
    }
    // Any other second spelling cannot be resolved from the DDL, so a mutation pair gets no verdict.
    let v = label(
        r#"DELETE FROM "s"."t" WHERE "a" = 1"#,
        r#"DELETE FROM "t" WHERE "a" = 1"#,
        ddl,
    );
    assert!(v.starts_with("NOT-COMPARABLE:"), "{v}");
}

#[test]
fn tables_of_one_name_in_two_schemas_hold_their_own_rows() {
    let ddl = r#"create table "s1"."t" ("a" INTEGER); create table "s2"."t" ("a" INTEGER)"#;
    assert_eq!(
        label(r#"SELECT "a" FROM "s1"."t""#, r#"SELECT "a" FROM "s2"."t""#, ddl),
        "NOT-EQUIVALENT"
    );
    // ... while one table, however it is spelled, is one table.
    assert_eq!(
        label(
            r#"SELECT "a" FROM "s1"."t""#,
            r#"SELECT "a" FROM "s1"."t" WHERE true"#,
            ddl
        ),
        "NO-COUNTEREXAMPLE"
    );
}

#[test]
fn every_frozen_clock_reads_one_instant() {
    let ddl = r#"create table "t" ("id" INTEGER, unique ("id"))"#;
    for (a, b) in [
        (r#"now()::time"#, "localtime"),
        (r#"now()::date"#, "current_date"),
        (r#"current_timestamp"#, "now()"),
        (r#"localtimestamp"#, "now()::timestamp"),
    ] {
        assert_eq!(
            label(
                &format!(r#"SELECT "id", {a} AS "x" FROM "t""#),
                &format!(r#"SELECT "id", {b} AS "x" FROM "t""#),
                ddl
            ),
            "NO-COUNTEREXAMPLE",
            "{a} / {b}"
        );
    }
}

#[test]
fn jsonb_build_object_keys_are_normalized() {
    let ddl = r#"create table "t" ("id" INTEGER, "a" INTEGER, unique ("id"))"#;
    for (a, b) in [
        (
            r#"jsonb_build_object('a', "a", 'b', "id")"#,
            r#"jsonb_build_object('b', "id", 'a', "a")"#,
        ),
        (
            r#"jsonb_build_object('a', "id", 'a', "a")"#,
            r#"jsonb_build_object('a', "a")"#,
        ),
    ] {
        assert_eq!(
            label(
                &format!(r#"SELECT "id", {a} AS "j" FROM "t""#),
                &format!(r#"SELECT "id", {b} AS "j" FROM "t""#),
                ddl
            ),
            "NO-COUNTEREXAMPLE",
            "{a} / {b}"
        );
    }
    // `json_build_object` keeps order and duplicates, as Postgres's `json` does.
    assert_eq!(
        label(
            r#"SELECT "id", json_build_object('a', "a", 'b', "id")::text AS "j" FROM "t""#,
            r#"SELECT "id", json_build_object('b', "id", 'a', "a")::text AS "j" FROM "t""#,
            ddl
        ),
        "NOT-EQUIVALENT"
    );
}

#[test]
fn a_record_compares_by_its_values_not_its_field_types() {
    let ddl = r#"create table "t" ("id" INTEGER, "b" BIGINT, unique ("id"))"#;
    assert_eq!(
        label(
            r#"SELECT "id", ROW("b") AS "r" FROM "t""#,
            r#"SELECT "id", ROW("b"::bigint) AS "r" FROM "t""#,
            ddl
        ),
        "NO-COUNTEREXAMPLE"
    );
    // ... and still by its values.
    assert_eq!(
        label(
            r#"SELECT "id", ROW("b") AS "r" FROM "t""#,
            r#"SELECT "id", ROW("b" + 1) AS "r" FROM "t""#,
            ddl
        ),
        "NOT-EQUIVALENT"
    );
}

#[test]
fn regex_operators_match_anywhere_and_similar_to_is_withheld() {
    let ddl = r#"create table "t" ("id" INTEGER, "c" TEXT, unique ("id"))"#;
    for (a, b) in [
        (r#""c" || 'x' ~ 'a'"#, r#""c" || 'x' LIKE '%a%'"#),
        (r#""c" ~* 'A'"#, r#""c" ILIKE '%a%'"#),
        (r#""c" !~ 'a'"#, r#""c" NOT LIKE '%a%'"#),
        (r#""c" !~* 'A'"#, r#""c" NOT ILIKE '%a%'"#),
    ] {
        assert_eq!(
            label(
                &format!(r#"SELECT "id" FROM "t" WHERE {a}"#),
                &format!(r#"SELECT "id" FROM "t" WHERE {b}"#),
                ddl
            ),
            "NO-COUNTEREXAMPLE",
            "{a} / {b}"
        );
    }
    let v = label(
        r#"SELECT "id" FROM "t" WHERE "c" SIMILAR TO 'a%'"#,
        r#"SELECT "id" FROM "t" WHERE "c" LIKE 'a%'"#,
        ddl,
    );
    assert!(v.starts_with("NOT-COMPARABLE:"), "{v}");
}
