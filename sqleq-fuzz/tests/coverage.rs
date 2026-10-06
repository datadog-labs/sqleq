// Unless explicitly stated otherwise all files in this repository are licensed under the
// Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/).
// Copyright 2026-Present Datadog, Inc.

//! Refutations the comparison used to throw away (issue #65): a `LIMIT` under a total order, a
//! pagination parameter bound so that it returned nothing, and a witness that needs an empty table.

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
fn a_limit_under_a_total_order_compares_the_rows() {
    // x and y are unique and NOT NULL, so each t row joins at most one u row: t.id and u.id are both
    // unique in the join, and each side keeps a determined row -- a different one.
    let ddl = r#"create table "t" ("id" INTEGER PRIMARY KEY, "x" INTEGER NOT NULL UNIQUE);
                 create table "u" ("id" INTEGER PRIMARY KEY, "y" INTEGER NOT NULL UNIQUE)"#;
    assert_eq!(
        label(
            r#"SELECT "t"."id" AS "tid", "u"."id" AS "uid" FROM "t" JOIN "u" ON "t"."x" = "u"."y" ORDER BY "t"."id" LIMIT 1"#,
            r#"SELECT "t"."id" AS "tid", "u"."id" AS "uid" FROM "t" JOIN "u" ON "t"."x" = "u"."y" ORDER BY "u"."id" LIMIT 1"#,
            ddl
        ),
        "NOT-EQUIVALENT"
    );
}

#[test]
fn a_limit_over_ties_is_still_cardinality_only() {
    // The control: `b` is not unique, so either side may keep either tied row.
    let ddl = r#"create table "t" ("id" INTEGER, "a" INTEGER, "b" INTEGER, unique ("id"))"#;
    assert_eq!(
        label(
            r#"SELECT "a" FROM "t" ORDER BY "b" LIMIT 1"#,
            r#"SELECT "a" FROM (SELECT "a", "b" FROM "t" ORDER BY "a" DESC) AS "s" ORDER BY "b" LIMIT 1"#,
            ddl
        ),
        "NO-COUNTEREXAMPLE"
    );
}

#[test]
fn a_pagination_offset_parameter_is_bound_to_zero() {
    let ddl = r#"create table "t" ("id" INTEGER PRIMARY KEY, "a" INTEGER)"#;
    for tail in ["LIMIT $1 OFFSET $2", "OFFSET $1", "LIMIT $1 OFFSET 0"] {
        assert_eq!(
            label(
                &format!(r#"SELECT "id", "a" FROM "t" ORDER BY "id" {tail}"#),
                &format!(r#"SELECT "id", "a" + 1 AS "a" FROM "t" ORDER BY "id" {tail}"#),
                ddl
            ),
            "NOT-EQUIVALENT",
            "{tail}"
        );
    }
}

#[test]
fn a_cut_on_one_side_only_is_seen() {
    // Binding the counts so that they cut nothing would make B the whole table, as A is; for any
    // OFFSET of 1 or more, or a LIMIT smaller than the table, it is not.
    let ddl = r#"create table "t" ("id" INTEGER PRIMARY KEY, "a" INTEGER)"#;
    for tail in ["LIMIT $1 OFFSET $2", "OFFSET $1", "LIMIT $1"] {
        assert_eq!(
            label(
                r#"SELECT "id", "a" FROM "t""#,
                &format!(r#"SELECT "id", "a" FROM "t" ORDER BY "id" {tail}"#),
                ddl
            ),
            "NOT-EQUIVALENT",
            "{tail}"
        );
    }
}

#[test]
fn an_empty_table_is_drawn() {
    // `sum` over no rows is NULL, so A is empty and B is not exactly when s is empty.
    let ddl = r#"create table "t" ("id" INTEGER, unique ("id"));
                 create table "s" ("y" INTEGER NOT NULL)"#;
    assert_eq!(
        label(
            r#"SELECT 1 AS "one" FROM "t" WHERE (SELECT sum("y") FROM "s") IS NOT NULL"#,
            r#"SELECT 1 AS "one" FROM "t""#,
            ddl
        ),
        "NOT-EQUIVALENT"
    );
    assert_eq!(
        label(
            r#"SELECT 1 AS "one" FROM "t" WHERE EXISTS (SELECT 1 FROM "s")"#,
            r#"SELECT 1 AS "one" FROM "t""#,
            ddl
        ),
        "NOT-EQUIVALENT"
    );
}
