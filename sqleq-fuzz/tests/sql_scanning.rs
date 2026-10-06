// Unless explicitly stated otherwise all files in this repository are licensed under the
// Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/).
// Copyright 2026-Present Datadog, Inc.

//! Placeholders, row cuts and function calls are read off the SQL's tokens or its parse, never off
//! its raw text (issue #63).

use std::collections::{BTreeSet, HashMap};

use sqleq_fuzz::gen::Val;
use sqleq_fuzz::patterns::{freeze_time, params_of, substitute};
use sqleq_fuzz::{test_pair, Config, Verdict};

const CFG: Config = Config {
    trials: 120,
    nrows: 5,
    seed: 0,
};

fn label(a: &str, b: &str, ddl: &str) -> String {
    test_pair(a, b, ddl, CFG).label()
}

#[test]
fn a_placeholder_spelled_inside_a_literal_is_not_one() {
    let ddl = r#"create table "t" ("id" INTEGER, unique ("id"))"#;
    assert_eq!(
        label(
            r#"SELECT '$1' AS "x" FROM "t""#,
            r#"SELECT '$' || '1' AS "x" FROM "t""#,
            ddl
        ),
        "NO-COUNTEREXAMPLE"
    );
    let binds: HashMap<u32, Val> = [(1, Val::Int(7))].into_iter().collect();
    assert_eq!(
        substitute("SELECT '$1', $1 /* $1 */, $$ $1 $$", &binds),
        "SELECT '$1', 7 /* $1 */, $$ $1 $$"
    );
    assert_eq!(
        params_of("SELECT '$2' FROM t WHERE a = $1 -- $3"),
        BTreeSet::from([1])
    );
}

#[test]
fn a_dollar_in_a_literal_does_not_misalign_the_pair() {
    // Both sides use $1 alone; '$2' is a string. B differs from A where s = 'a'.
    let ddl = r#"create table "t" ("id" INTEGER, "a" INTEGER, "s" TEXT, unique ("id"))"#;
    assert_eq!(
        label(
            r#"SELECT "id" FROM "t" WHERE "a" = $1 AND "s" <> '$2'"#,
            r#"SELECT "id" FROM "t" WHERE "a" = $1 AND "s" <> 'a'"#,
            ddl
        ),
        "NOT-EQUIVALENT"
    );
}

/// Every spelling of a one-row cut over tied rows keeps an arbitrary row, so only cardinality may be
/// compared. Postgres itself returns different rows for these two queries on one instance.
#[test]
fn every_spelling_of_a_cut_over_ties_is_cardinality_only() {
    let ddl = r#"create table "t" ("id" INTEGER, "a" INTEGER, "b" INTEGER, unique ("id"))"#;
    for cut in ["LIMIT (1)", "FETCH FIRST ROW ONLY", "FETCH NEXT ROWS ONLY", "LIMIT ($1)"] {
        let a = format!(r#"SELECT "a" FROM "t" ORDER BY "b" {cut}"#);
        let b = format!(
            r#"SELECT "a" FROM (SELECT "a", "b" FROM "t" ORDER BY "a" DESC) AS "s" ORDER BY "b" {cut}"#
        );
        assert_eq!(label(&a, &b, ddl), "NO-COUNTEREXAMPLE", "{cut}");
    }
}

#[test]
fn every_volatile_function_skips_the_pair() {
    let ddl = r#"create table "t" ("id" INTEGER NOT NULL, unique ("id"))"#;
    for f in ["uuidv7()", "uuidv4()", "txid_current()", "pg_catalog.random()", "lastval()"] {
        let q = format!(r#"SELECT "id", {f} AS "x" FROM "t""#);
        assert!(
            matches!(test_pair(&q, &q, ddl, CFG), Verdict::NondetSkip),
            "{f}"
        );
    }
}

#[test]
fn a_volatile_name_that_is_not_a_call_does_not_skip_the_pair() {
    let ddl = r#"create table "t" ("id" INTEGER NOT NULL, "random" INTEGER, unique ("id"))"#;
    assert_eq!(
        label(
            r#"SELECT "id", 'random()' AS "x", "random" FROM "t""#,
            r#"SELECT "id", 'random()' AS "x", "random" FROM "t" WHERE "random" > 0"#,
            ddl
        ),
        "NOT-EQUIVALENT"
    );
}

#[test]
fn a_placeholder_past_u32_is_an_error_not_a_panic() {
    let v = test_pair(
        "SELECT id FROM t WHERE a = $4294967296",
        "SELECT id FROM t WHERE a = $4294967296",
        "create table t (id INTEGER, a INTEGER, unique (id))",
        CFG,
    );
    match v {
        Verdict::Error(e) => assert!(e.contains("no parameter"), "{e}"),
        other => panic!("{other:?}"),
    }
    // `$0` is no parameter either.
    assert!(matches!(
        test_pair(
            "SELECT id FROM t WHERE a = $0",
            "SELECT id FROM t WHERE a = $0",
            "create table t (id INTEGER, a INTEGER)",
            CFG
        ),
        Verdict::Error(_)
    ));
}

#[test]
fn a_frozen_clock_in_a_literal_stays_text() {
    assert_eq!(
        freeze_time("SELECT 'now()', now() -- current_date"),
        "SELECT 'now()', TIMESTAMP '2020-06-01 00:00:00' -- current_date"
    );
}
