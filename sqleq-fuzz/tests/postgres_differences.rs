// Unless explicitly stated otherwise all files in this repository are licensed under the
// Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/).
// Copyright 2026-Present Datadog, Inc.

//! More places DuckDB computes something Postgres does not (issue #89), and the sequence columns
//! it filled with NULLs (issue #94).
//!
//! Each equivalent pair here was refuted: DuckDB divided a `numeric` into a DOUBLE, read a `LIKE`
//! pattern with no escape character, answered `inf` or `NaN` where Postgres raises, compared `jsonb`
//! literals as text, compared an arbitrary choice among tied rows as a bag, materialized an
//! `interval` column as an INTEGER, printed a DOUBLE as `2.0`, compared intervals by their fields,
//! and drew NULLs into `serial` and identity columns. The non-equivalent ones check that what fixed
//! each still refutes what it should.

use sqleq_fuzz::{test_pair, Config};

/// The budget `sqleq-check` passes, so a verdict here is the one a pinned pair records.
const CFG: Config = Config {
    trials: 120,
    nrows: 5,
    seed: 0,
};

fn label(a: &str, b: &str, ddl: &str) -> String {
    test_pair(a, b, ddl, CFG).label()
}

/// The part of a label before its `:`, which is what a pinned pair records.
fn kind(a: &str, b: &str, ddl: &str) -> String {
    label(a, b, ddl)
        .split(':')
        .next()
        .unwrap_or_default()
        .to_string()
}

const T: &str = r#"create table "t" ("id" INTEGER, "a" INTEGER, unique ("id"))"#;

// -- 1. A `numeric` division is a DOUBLE in DuckDB ------------------------------------------------

#[test]
fn a_numeric_division_gets_no_verdict() {
    let ddl = r#"create table "t" ("id" INTEGER, "n" NUMERIC(10,0), unique ("id"))"#;
    // 2 / 3 * 3 is 2 in a DOUBLE and 2.00000000000000000001 in Postgres.
    assert_eq!(
        kind(
            r#"SELECT "id" FROM "t" WHERE "n" / 3 * 3 = "n""#,
            r#"SELECT "id" FROM "t" WHERE "n" % 3 = 0"#,
            ddl
        ),
        "NOT-COMPARABLE"
    );
    // A numeric literal, an alias of a numeric column and an average are numeric divisions too.
    for (a, b) in [
        (r#"SELECT "a" / 2.0 FROM "t""#, r#"SELECT "a" * 0.5 FROM "t""#),
        (
            r#"SELECT "m" / 3 FROM (SELECT CAST("a" AS NUMERIC) AS "m" FROM "t") AS "s""#,
            r#"SELECT "a" FROM "t""#,
        ),
        (r#"SELECT avg("a") / 3 FROM "t""#, r#"SELECT 1 FROM "t""#),
    ] {
        assert_eq!(kind(a, b, T), "NOT-COMPARABLE", "{a}");
    }
}

#[test]
fn integer_and_float_divisions_are_still_compared() {
    // Integer division truncates, as in Postgres, and a float division is a DOUBLE in both.
    assert_eq!(
        label(
            r#"SELECT "id", "a" / 2 AS "h" FROM "t""#,
            r#"SELECT "id", "a" * 0.5 AS "h" FROM "t""#,
            T
        ),
        "NOT-EQUIVALENT"
    );
    assert_eq!(
        label(
            r#"SELECT "id", CAST("a" AS DOUBLE PRECISION) / 2 AS "h" FROM "t""#,
            r#"SELECT "id", CAST("a" AS DOUBLE PRECISION) * 0.5 AS "h" FROM "t""#,
            T
        ),
        "NO-COUNTEREXAMPLE"
    );
}

// -- 2. `LIKE` escapes with a backslash in Postgres -----------------------------------------------

const S: &str = r#"create table "t" ("id" INTEGER, "s" TEXT, unique ("id"))"#;

#[test]
fn a_like_pattern_escapes_with_a_backslash() {
    for (a, b) in [
        (
            r#"SELECT "id" FROM "t" WHERE "s" LIKE '\a'"#,
            r#"SELECT "id" FROM "t" WHERE "s" = 'a'"#,
        ),
        (
            r#"SELECT "id" FROM "t" WHERE "s" NOT ILIKE '\A'"#,
            r#"SELECT "id" FROM "t" WHERE NOT "s" ILIKE 'a'"#,
        ),
        (
            r#"SELECT "id" FROM "t" WHERE "s" LIKE '\a%'"#,
            r#"SELECT "id" FROM "t" WHERE "s" LIKE 'a%'"#,
        ),
    ] {
        assert_eq!(label(a, b, S), "NO-COUNTEREXAMPLE", "{a}");
    }
    // ... and the escape still means something: `\a` is not the string `\a`.
    assert_eq!(
        label(
            r#"SELECT "id" FROM "t" WHERE "s" LIKE '\a'"#,
            r#"SELECT "id" FROM "t" WHERE "s" = '\a'"#,
            S
        ),
        "NOT-EQUIVALENT"
    );
    // An `ESCAPE` of the query's own is left as it is.
    assert_eq!(
        label(
            r#"SELECT "id" FROM "t" WHERE "s" LIKE '!a' ESCAPE '!'"#,
            r#"SELECT "id" FROM "t" WHERE "s" = 'a'"#,
            S
        ),
        "NO-COUNTEREXAMPLE"
    );
}

#[test]
fn a_like_operator_that_takes_no_escape_clause_gets_no_verdict() {
    // `~~` is Postgres's LIKE, backslash escape and all, and takes no ESCAPE clause to say so.
    assert_eq!(
        kind(
            r#"SELECT "id" FROM "t" WHERE "s" ~~ '\a'"#,
            r#"SELECT "id" FROM "t" WHERE "s" = 'a'"#,
            S
        ),
        "NOT-COMPARABLE"
    );
    // A pattern with nothing escaped means the same either way.
    assert_eq!(
        label(
            r#"SELECT "id" FROM "t" WHERE "s" ~~ 'a%'"#,
            r#"SELECT "id" FROM "t" WHERE "s" LIKE 'a%'"#,
            S
        ),
        "NO-COUNTEREXAMPLE"
    );
}

// -- 3. `power` and `exp` raise where Postgres raises ---------------------------------------------

#[test]
fn power_and_exp_raise_where_postgres_raises() {
    for (a, b) in [
        // A zero base and a negative exponent.
        (
            r#"SELECT "id", power("a", -1) AS "r" FROM "t""#,
            r#"SELECT "id", -power(-"a", -1) AS "r" FROM "t""#,
        ),
        // An overflow.
        (
            r#"SELECT "id" FROM "t" WHERE exp(CAST("a" AS DOUBLE PRECISION) * 1000) > 0"#,
            r#"SELECT "id" FROM "t" WHERE "a" = 0"#,
        ),
        // A negative base and a fractional exponent, over double precision.
        (
            r#"SELECT "id" FROM "t" WHERE pow(CAST("a" - 1 AS DOUBLE PRECISION), 0.5::float8) >= 0"#,
            r#"SELECT "id" FROM "t" WHERE "a" >= 1"#,
        ),
    ] {
        assert_eq!(kind(a, b, T), "NO-COUNTEREXAMPLE", "{a}");
    }
    // ... and where it does not raise they still compute.
    assert_eq!(
        label(
            r#"SELECT "id", power("a", 2) AS "r" FROM "t""#,
            r#"SELECT "id", power("a", 3) AS "r" FROM "t""#,
            T
        ),
        "NOT-EQUIVALENT"
    );
    assert_eq!(
        label(
            r#"SELECT "id", exp("a") AS "r" FROM "t""#,
            r#"SELECT "id", exp("a" + 0) AS "r" FROM "t""#,
            T
        ),
        "NO-COUNTEREXAMPLE"
    );
}

#[test]
fn power_of_a_numeric_and_the_caret_get_no_verdict() {
    // `0.5` is a numeric, so this is Postgres's numeric power, computed in a DOUBLE by DuckDB.
    assert_eq!(
        kind(
            r#"SELECT "id" FROM "t" WHERE power("a" - 1, 0.5) >= 0"#,
            r#"SELECT "id" FROM "t" WHERE "a" >= 1"#,
            T
        ),
        "NOT-COMPARABLE"
    );
    assert_eq!(
        kind(
            r#"SELECT "id", "a" ^ -1 AS "r" FROM "t""#,
            r#"SELECT "id", -((-"a") ^ -1) AS "r" FROM "t""#,
            T
        ),
        "NOT-COMPARABLE"
    );
}

// -- 4. `jsonb` literals are one value however they are spelled ------------------------------------

const J: &str = r#"create table "t" ("id" INTEGER, "j" JSONB, unique ("id"))"#;

#[test]
fn a_jsonb_literal_matches_however_it_is_spelled() {
    for b in [
        r#"SELECT "id" FROM "t" WHERE "j" = '{"b":"b","a":1}'"#,
        r#"SELECT "id" FROM "t" WHERE "j" = '{"a": 1, "b": "b"}'"#,
        r#"SELECT "id" FROM "t" WHERE "j" IN ('{"b": "b", "a": 1}')"#,
        r#"SELECT "id" FROM "t" WHERE "j" = CAST('{"a":0,"b":"b","a":1}' AS JSONB)"#,
    ] {
        assert_eq!(
            label(r#"SELECT "id" FROM "t" WHERE "j" = '{"a":1,"b":"b"}'"#, b, J),
            "NO-COUNTEREXAMPLE",
            "{b}"
        );
    }
    // ... and a different document is still a different document.
    assert_eq!(
        label(
            r#"SELECT "id" FROM "t" WHERE "j" = '{"a":1,"b":"b"}'"#,
            r#"SELECT "id" FROM "t" WHERE "j" = '{"b": "c", "a": 1}'"#,
            J
        ),
        "NOT-EQUIVALENT"
    );
    // A written document is compared by value too.
    assert_eq!(
        label(
            r#"INSERT INTO "t" VALUES (7, '{"b":"b", "a":1}')"#,
            r#"INSERT INTO "t" ("j", "id") VALUES ('{"a":1,"b":"b"}', 7)"#,
            J
        ),
        "NO-COUNTEREXAMPLE"
    );
}

#[test]
fn a_jsonb_literal_with_no_exact_spelling_gets_no_verdict() {
    // `1.0` and `1` are one jsonb number.
    assert_eq!(
        kind(
            r#"SELECT "id" FROM "t" WHERE "j" = '{"a":1.0,"b":"b"}'"#,
            r#"SELECT "id" FROM "t" WHERE "j" = '{"a":1,"b":"b"}'"#,
            J
        ),
        "NOT-COMPARABLE"
    );
    // A JSON document where this cannot tell what it meets.
    assert_eq!(
        kind(
            r#"SELECT "id" FROM "t" WHERE "j" = (CASE WHEN "id" > 0 THEN '{"b":"b","a":1}' END)::jsonb"#,
            r#"SELECT "id" FROM "t" WHERE "j" = '{"a":1,"b":"b"}' AND "id" > 0"#,
            J
        ),
        "NOT-COMPARABLE"
    );
}

// -- 5. Ties inside `DISTINCT ON` and in a window `ORDER BY` --------------------------------------

const G: &str = r#"create table "t" ("id" INTEGER, "g" INTEGER, "v" INTEGER, unique ("id"))"#;

#[test]
fn a_tie_inside_distinct_on_is_compared_by_cardinality() {
    assert_eq!(
        label(
            r#"SELECT DISTINCT ON ("g") "g", "v" FROM "t" ORDER BY "g""#,
            r#"SELECT DISTINCT ON ("g") "g", "v" FROM (SELECT * FROM "t" ORDER BY "v" DESC) AS "s" ORDER BY "g""#,
            G
        ),
        "NO-COUNTEREXAMPLE"
    );
    // The number of keys is still compared...
    assert_eq!(
        label(
            r#"SELECT DISTINCT ON ("g") "g", "v" FROM "t" ORDER BY "g""#,
            r#"SELECT DISTINCT ON ("v") "g", "v" FROM "t" ORDER BY "v""#,
            G
        ),
        "NOT-EQUIVALENT"
    );
    // ... and a row that is determined is compared whole: the order picks one per key.
    let ddl = r#"create table "t" ("g" INTEGER, "v" INTEGER)"#;
    assert_eq!(
        label(
            r#"SELECT DISTINCT ON ("g") "g", "v" FROM "t" ORDER BY "g", "v""#,
            r#"SELECT DISTINCT ON ("g") "g", "v" FROM "t" ORDER BY "g", "v" DESC"#,
            ddl
        ),
        "NOT-EQUIVALENT"
    );
}

#[test]
fn a_tie_inside_a_nested_distinct_on_or_a_window_gets_no_comparison() {
    assert_eq!(
        label(
            r#"SELECT "id", row_number() OVER (ORDER BY "g") AS "rn" FROM "t""#,
            r#"SELECT "id", row_number() OVER (ORDER BY "g") AS "rn" FROM (SELECT * FROM "t" ORDER BY "id" DESC) AS "s""#,
            G
        ),
        "NONDET-SKIP"
    );
    // Which row a nested DISTINCT ON keeps decides what the level above it filters.
    assert_eq!(
        label(
            r#"SELECT * FROM (SELECT DISTINCT ON ("g") "g", "v" FROM "t" ORDER BY "g") AS "s" WHERE "v" > 0"#,
            r#"SELECT * FROM (SELECT DISTINCT ON ("g") "g", "v" FROM (SELECT * FROM "t" ORDER BY "v") AS "u" ORDER BY "g") AS "s" WHERE "v" > 0"#,
            G
        ),
        "NONDET-SKIP"
    );
    // A window over a total order is compared as before.
    let ddl = r#"create table "t" ("id" INTEGER NOT NULL, "g" INTEGER, unique ("id"))"#;
    assert_eq!(
        label(
            r#"SELECT "id", row_number() OVER (ORDER BY "g", "id") AS "rn" FROM "t""#,
            r#"SELECT "id", row_number() OVER (ORDER BY "g" DESC, "id") AS "rn" FROM "t""#,
            ddl
        ),
        "NOT-EQUIVALENT"
    );
}

// -- 6. An `interval` column is not an INTEGER ----------------------------------------------------

#[test]
fn a_pair_reading_an_interval_column_gets_no_verdict() {
    let ddl = r#"create table "t" ("id" INTEGER, "x" INTERVAL, unique ("id"))"#;
    assert_eq!(
        kind(
            r#"SELECT "id", "x" / 2 AS "h" FROM "t""#,
            r#"SELECT "id", "x" * 0.5 AS "h" FROM "t""#,
            ddl
        ),
        "NOT-COMPARABLE"
    );
    // One that does not read it is still compared.
    assert_eq!(
        label(
            r#"SELECT "id" FROM "t" WHERE "id" > 0"#,
            r#"SELECT "id" FROM "t" WHERE "id" >= 1"#,
            ddl
        ),
        "NO-COUNTEREXAMPLE"
    );
}

// -- 7. A `double precision` printed as text --------------------------------------------------------

#[test]
fn a_float_turned_into_text_gets_no_verdict() {
    for a in [
        r#"SELECT "id", CAST(CAST("a" AS DOUBLE PRECISION) AS TEXT) AS "s" FROM "t""#,
        r#"SELECT "id", "a"::float8::varchar AS "s" FROM "t""#,
        r#"SELECT "id", CAST("a" AS DOUBLE PRECISION) || '' AS "s" FROM "t""#,
        r#"SELECT "id", concat(CAST("a" AS REAL)) AS "s" FROM "t""#,
    ] {
        assert_eq!(
            kind(a, r#"SELECT "id", CAST("a" AS TEXT) AS "s" FROM "t""#, T),
            "NOT-COMPARABLE",
            "{a}"
        );
    }
    // An integer printed as text is still compared.
    assert_eq!(
        label(
            r#"SELECT "id", CAST("a" AS TEXT) AS "s" FROM "t""#,
            r#"SELECT "id", CAST("a" + 1 AS TEXT) AS "s" FROM "t""#,
            T
        ),
        "NOT-EQUIVALENT"
    );
}

// -- 8. Intervals are compared by `=` -------------------------------------------------------------

#[test]
fn intervals_equal_under_equality_are_one_value() {
    let ddl = r#"create table "t" ("id" INTEGER, unique ("id"))"#;
    for (a, b) in [
        ("SELECT INTERVAL '1 day' FROM t", "SELECT INTERVAL '24 hours' FROM t"),
        ("SELECT INTERVAL '1 month' FROM t", "SELECT INTERVAL '30 days' FROM t"),
        (
            "SELECT ARRAY[INTERVAL '1 day'] FROM t",
            "SELECT ARRAY[INTERVAL '86400 seconds'] FROM t",
        ),
    ] {
        assert_eq!(label(a, b, ddl), "NO-COUNTEREXAMPLE", "{a}");
    }
    assert_eq!(
        label(
            "SELECT INTERVAL '1 day' FROM t",
            "SELECT INTERVAL '25 hours' FROM t",
            ddl
        ),
        "NOT-EQUIVALENT"
    );
}

// -- #94. `serial` and identity columns are NOT NULL ------------------------------------------------

#[test]
fn a_sequence_column_is_never_null() {
    for ddl in [
        r#"create table "t" ("id" SERIAL, "a" INTEGER)"#,
        r#"create table "t" ("id" BIGSERIAL, "a" INTEGER)"#,
        r#"create table "t" ("id" smallserial, "a" INTEGER)"#,
        r#"create table "t" ("id" serial8, "a" INTEGER)"#,
        r#"create table "t" ("id" INTEGER GENERATED ALWAYS AS IDENTITY, "a" INTEGER)"#,
        r#"create table "t" ("id" BIGINT GENERATED BY DEFAULT AS IDENTITY (START WITH 10), "a" INTEGER)"#,
    ] {
        assert_eq!(
            label(
                r#"SELECT "a" FROM "t" WHERE "id" IS NOT NULL"#,
                r#"SELECT "a" FROM "t""#,
                ddl
            ),
            "NO-COUNTEREXAMPLE",
            "{ddl}"
        );
    }
}

#[test]
fn a_sequence_column_draws_distinct_values() {
    // Each value at most once, so no two rows share an id; the pair is not equivalent in Postgres
    // (an explicit value may repeat one), which is why this is not a key.
    let ddl = r#"create table "t" ("id" SERIAL, "a" INTEGER)"#;
    assert_eq!(
        label(
            r#"SELECT count(*) FROM "t""#,
            r#"SELECT count(DISTINCT "id") FROM "t""#,
            ddl
        ),
        "NO-COUNTEREXAMPLE"
    );
    // ... and the values are still drawn.
    assert_eq!(
        label(
            r#"SELECT "a" FROM "t" WHERE "id" > 0"#,
            r#"SELECT "a" FROM "t""#,
            ddl
        ),
        "NOT-EQUIVALENT"
    );
}
