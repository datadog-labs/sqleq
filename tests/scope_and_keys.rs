// Unless explicitly stated otherwise all files in this repository are licensed under the
// Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/).
// Copyright 2026-Present Datadog, Inc.

//! Three rules of the catalog and of name resolution:
//!
//! * a reference that misses the named columns of a relation with an unaliased, unnamed column (a
//!   `CASE`, a cast of an expression, a scalar subquery, a `VALUES` column) may be that column in
//!   Postgres, so it is refused rather than resolved in an enclosing query; a qualified name reads
//!   the nearest relation of that name or nothing; and the placeholder name of such a column is not
//!   something a query can spell (issue #88);
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

/// A name that may be an unnamed column, missed by the frontend.
const UNKNOWN: &str = "whose name is not known";

// --- #88: an unnamed column of a derived table -----------------------------------------------

/// `t` is the derived table's source, `u` the enclosing query's table, with columns named the way
/// Postgres names the derived table's items: `case`, `text`, `max` and `column1`.
const DERIVED: &str = r#"create table "t" ("v" INTEGER);
create table "u" ("id" INTEGER, "case" INTEGER, "text" TEXT, "max" INTEGER, "column1" INTEGER);"#;

/// `EXISTS` over a derived table whose one column is `item`, filtered by `cond`.
fn exists_over(item: &str, cond: &str) -> String {
    format!(r#"SELECT "id" FROM "u" WHERE EXISTS (SELECT 1 FROM (SELECT {item} FROM "t") AS "s" WHERE {cond})"#)
}

/// Postgres names an unaliased `CASE` `case`, so a bare `"case"` is the derived table's column.
/// `u = {(1, 1, ..)}`, `t = {(-1)}`: `s."case"` is 0, so the first returns no rows; the second tests
/// `u."case" = 1` and returns 1. The bare name used to miss the placeholder and reach `u."case"`.
#[test]
fn a_bare_name_that_may_be_a_case_column_does_not_reach_the_enclosing_query() {
    let case = r#"CASE WHEN "v" > 0 THEN 1 ELSE 0 END"#;
    let (a, b) = (exists_over(case, r#""case" = 1"#), exists_over(case, r#""u"."case" = 1"#));
    refused(DERIVED, &a, &b, "unsupported", UNKNOWN);
}

/// A cast of an expression is named after its type: `CAST(v + 1 AS TEXT)` is `text`.
/// `u = {(1, .., '1', ..)}`, `t = {(5)}`: `s.text` is `'6'`, so the first returns no rows and the
/// second returns 1.
#[test]
fn a_bare_name_that_may_be_a_cast_of_an_expression_does_not_reach_the_enclosing_query() {
    let cast = r#"CAST("v" + 1 AS TEXT)"#;
    let (a, b) = (exists_over(cast, r#""text" = '1'"#), exists_over(cast, r#""u"."text" = '1'"#));
    refused(DERIVED, &a, &b, "unsupported", UNKNOWN);
}

/// A scalar subquery is named after what it selects when that is a call: here `max`.
/// `u = {(1, .., 1, ..)}`, `t = {(2)}`: `s.max` is 2, so the first returns no rows and the second 1.
#[test]
fn a_bare_name_that_may_be_a_scalar_subquery_does_not_reach_the_enclosing_query() {
    let sub = r#"(SELECT max("v") FROM "t")"#;
    let (a, b) = (exists_over(sub, r#""max" = 1"#), exists_over(sub, r#""u"."max" = 1"#));
    refused(DERIVED, &a, &b, "unsupported", UNKNOWN);
}

/// Postgres names the columns of `VALUES` `column1`, `column2`, ...
/// `u = {(1, .., 1)}`: `s.column1` is 2, so the first returns no rows and the second 1.
#[test]
fn a_bare_name_that_may_be_a_values_column_does_not_reach_the_enclosing_query() {
    let values = |cond: &str| {
        format!(r#"SELECT "id" FROM "u" WHERE EXISTS (SELECT 1 FROM (VALUES (2)) AS "s" WHERE {cond})"#)
    };
    refused(DERIVED, &values(r#""column1" = 1"#), &values(r#""u"."column1" = 1"#), "unsupported", UNKNOWN);
}

/// The same holds for a `GROUP BY` name, which falls back to a select-list alias only when no
/// input column could be meant. `t = {(-1), (1)}`: the first groups by `s."case"` and returns
/// `(1, 1)` twice, the second groups by a constant and returns `(1, 2)`. The first used to fall back
/// to its alias `"case"`, the constant 1.
#[test]
fn a_group_by_name_that_may_be_an_unnamed_column_does_not_fall_back_to_an_alias() {
    let q = |key: &str| {
        format!(
            r#"SELECT 1 AS "case", count(*) FROM (SELECT CASE WHEN "v" > 0 THEN 1 ELSE 0 END FROM "t") AS "s" GROUP BY {key}"#
        )
    };
    refused(DERIVED, &q(r#""case""#), &q("2 - 1"), "unsupported", UNKNOWN);
}

/// A qualified `s.x` reads the nearest relation named `s`, and Postgres raises an error when that
/// one has no column `x` ("column s.x does not exist"). It used to go on to the enclosing `t AS s`.
/// On `t = {(1, 1), (2, 2)}` the second returns 1.
#[test]
fn a_qualified_name_stops_at_the_nearest_relation_of_that_name() {
    let ddl = r#"create table "t" ("id" INTEGER, "x" INTEGER);"#;
    refused(
        ddl,
        r#"SELECT "s"."id" FROM "t" AS "s" WHERE EXISTS (SELECT 1 FROM (SELECT "x" + 0 FROM "t") AS "s" WHERE "s"."x" = 1)"#,
        r#"SELECT "o"."id" FROM "t" AS "o" WHERE EXISTS (SELECT 1 FROM (SELECT "x" + 0 FROM "t") AS "s" WHERE "o"."x" = 1)"#,
        "schema",
        "unresolved column s.x",
    );
    // When the nearest `s` has an unnamed column, the name may be that column: Postgres reads
    // `s.text` as the cast below.
    refused(
        DERIVED,
        r#"SELECT "s"."id" FROM "u" AS "s" WHERE EXISTS (SELECT 1 FROM (SELECT CAST("v" + 1 AS TEXT) FROM "t") AS "s" WHERE "s"."text" = '1')"#,
        r#"SELECT "o"."id" FROM "u" AS "o" WHERE EXISTS (SELECT 1 FROM (SELECT CAST("v" + 1 AS TEXT) FROM "t") AS "s" WHERE "o"."text" = '1')"#,
        "unsupported",
        UNKNOWN,
    );
}

/// The frontend's placeholder for an unnamed column is not a name a query can reach. Postgres
/// rejects the first query (`column "$col0" does not exist`); it used to lower as the second.
#[test]
fn an_unnamed_column_cannot_be_spelled() {
    let ddl = r#"create table "t" ("id" INTEGER, "x" INTEGER);"#;
    let case = r#"CASE WHEN "x" > 0 THEN 1 ELSE 0 END"#;
    for spelled in [r#""$col0""#, r#""s"."$col0""#] {
        let q0 = format!(r#"SELECT {spelled} FROM (SELECT {case} FROM "t") AS "s""#);
        let q1 = format!(r#"SELECT {case} FROM "t""#);
        refused(ddl, &q0, &q1, "unsupported", UNKNOWN);
    }
}

/// Controls: a name that resolves is unaffected. The derived table's named column (a cast of a
/// column is named after the column), a name the same query's other relation has beside an unnamed
/// column, a correlated name with no unnamed column on the way, and a qualified name with no nearer
/// relation of that name all lower, and alike where they are one query.
#[test]
fn names_that_resolve_still_lower() {
    // `s.v` and a bare `v` are the cast, in both.
    assert!(identical(
        DERIVED,
        r#"SELECT "id" FROM "u" WHERE EXISTS (SELECT 1 FROM (SELECT CAST("v" AS TEXT) FROM "t") AS "s" WHERE "v" = '1')"#,
        r#"SELECT "id" FROM "u" WHERE EXISTS (SELECT 1 FROM (SELECT CAST("v" AS TEXT) FROM "t") AS "s" WHERE "s"."v" = '1')"#,
    ));
    // `id` is `u`'s, beside the derived table's unnamed column in the same FROM.
    assert!(identical(
        DERIVED,
        r#"SELECT "id" FROM (SELECT CASE WHEN "v" > 0 THEN 1 ELSE 0 END FROM "t") AS "s", "u""#,
        r#"SELECT "u"."id" FROM (SELECT CASE WHEN "v" > 0 THEN 1 ELSE 0 END FROM "t") AS "s", "u""#,
    ));
    // A correlated bare name, and a qualified one reaching the enclosing query.
    assert!(identical(
        DERIVED,
        r#"SELECT "id" FROM "u" WHERE EXISTS (SELECT 1 FROM "t" WHERE "t"."v" = "max")"#,
        r#"SELECT "id" FROM "u" WHERE EXISTS (SELECT 1 FROM "t" WHERE "t"."v" = "u"."max")"#,
    ));
    // A `GROUP BY` alias no input column has still falls back.
    assert!(identical(
        DERIVED,
        r#"SELECT "v" + 1 AS "w", count(*) FROM "t" GROUP BY "w""#,
        r#"SELECT "v" + 1 AS "w", count(*) FROM "t" GROUP BY "v" + 1"#,
    ));
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
    // Each side reads `"X"` as well: type inference keeps the two aliases apart, and the seeded
    // catalog refuses a declared table no column is attributed to (issue #111).
    assert!(identical(
        QUOTED,
        &format!(r#"SELECT X.a, "X".a {from}"#),
        &format!(r#"SELECT "x".a, "X".a {from}"#)
    ));
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
