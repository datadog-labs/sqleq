// Unless explicitly stated otherwise all files in this repository are licensed under the
// Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/).
// Copyright 2026-Present Datadog, Inc.

//! DuckDB evaluates the pair as Postgres would, or the pair gets no verdict (issue #62).
//!
//! Each equivalent pair here was refuted when DuckDB ran with its own defaults: floating-point
//! integer division, NULLs last under `DESC`, the host's time zone, `inf` for a zero divisor,
//! `char(n)` as a VARCHAR, and `numeric` as a DOUBLE or a `DECIMAL(18,3)`. The non-equivalent ones
//! were missed for the same reasons.

use sqleq_fuzz::duck::open_db;
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

const T: &str = r#"create table "t" ("id" INTEGER, "a" INTEGER, unique ("id"))"#;

#[test]
fn integer_division_truncates_like_postgres() {
    // `a - a % 2` is the even integer `2 * (a / 2)`, so halving it is exact.
    assert_eq!(
        label(
            r#"SELECT "id", "a" / 2 AS "h" FROM "t""#,
            r#"SELECT "id", ("a" - "a" % 2) / 2 AS "h" FROM "t""#,
            T
        ),
        "NO-COUNTEREXAMPLE"
    );
}

#[test]
fn integer_division_is_told_apart_from_numeric_division() {
    // At a = 1 Postgres gives 0 for A and 0.5 for B; DuckDB's default gave 0.5 for both. (B is a
    // product: a numeric *division* is a DOUBLE in DuckDB, so a pair with one gets no verdict.)
    assert_eq!(
        label(
            r#"SELECT "id", "a" / 2 AS "h" FROM "t""#,
            r#"SELECT "id", "a" * 0.5 AS "h" FROM "t""#,
            T
        ),
        "NOT-EQUIVALENT"
    );
}

#[test]
fn desc_sorts_nulls_first_like_postgres() {
    let ddl = r#"create table "t" ("id" INTEGER NOT NULL, "a" INTEGER, unique ("id"))"#;
    assert_eq!(
        label(
            r#"SELECT "id", row_number() OVER (ORDER BY "a" DESC, "id") AS "rn" FROM "t""#,
            r#"SELECT "id", row_number() OVER (ORDER BY "a" DESC NULLS FIRST, "id") AS "rn" FROM "t""#,
            ddl
        ),
        "NO-COUNTEREXAMPLE"
    );
    // ... and so `DESC` is not `DESC NULLS LAST`.
    assert_eq!(
        label(
            r#"SELECT "id", row_number() OVER (ORDER BY "a" DESC, "id") AS "rn" FROM "t""#,
            r#"SELECT "id", row_number() OVER (ORDER BY "a" DESC NULLS LAST, "id") AS "rn" FROM "t""#,
            ddl
        ),
        "NOT-EQUIVALENT"
    );
}

#[test]
fn the_session_time_zone_is_fixed_not_the_hosts() {
    let con = open_db().unwrap();
    let tz: String = con
        .query_row("SELECT current_setting('TimeZone')", [], |r| r.get(0))
        .unwrap();
    assert_eq!(tz, "UTC");
}

#[test]
fn a_timestamptz_converts_to_local_time_like_postgres() {
    // For a timestamptz, `AT TIME ZONE` gives the local time in that zone; on a naive timestamp it
    // goes the other way. The New York calendar date is 2020-01-01 exactly on this UTC interval.
    let ddl = r#"create table "t" ("id" INTEGER, "ts" TIMESTAMPTZ, unique ("id"))"#;
    assert_eq!(
        label(
            r#"SELECT "id" FROM "t" WHERE ("ts" AT TIME ZONE 'America/New_York')::date = DATE '2020-01-01'"#,
            r#"SELECT "id" FROM "t" WHERE "ts" >= TIMESTAMPTZ '2020-01-01 05:00:00+00' AND "ts" < TIMESTAMPTZ '2020-01-02 05:00:00+00'"#,
            ddl
        ),
        "NO-COUNTEREXAMPLE"
    );
}

#[test]
fn a_zero_divisor_raises_on_both_sides() {
    // At a = 0 Postgres raises on both sides, so that trial compares nothing; DuckDB answered
    // inf and -inf. (A double precision dividend: a numeric one, `1.0 / a`, is a division DuckDB
    // computes in a DOUBLE, and a pair with one gets no verdict at all.)
    assert_eq!(
        label(
            r#"SELECT "id", CAST(1 AS DOUBLE PRECISION) / "a" AS "x" FROM "t""#,
            r#"SELECT "id", -CAST(1 AS DOUBLE PRECISION) / -"a" AS "x" FROM "t""#,
            T
        ),
        "NO-COUNTEREXAMPLE"
    );
    // `%` and `mod()`, which DuckDB answered with NULL, and a divisor that ends in a cast, which
    // sqlparser does not span. Postgres raises at a = 0 in A, so the CASE in B never decides
    // anything; DuckDB used to compare A's NULL with B's 0 there.
    for a in [
        r#"SELECT "id", 7 % "a" AS "x" FROM "t""#,
        r#"SELECT "id", mod(7, "a") AS "x" FROM "t""#,
        r#"SELECT "id", 7 / "a"::bigint AS "x" FROM "t""#,
    ] {
        let b = a.replace(
            r#"SELECT "id", "#,
            r#"SELECT "id", CASE WHEN "a" = 0 THEN 0 ELSE "#,
        )
        .replace(r#" AS "x""#, r#" END AS "x""#);
        assert_eq!(label(a, &b, T), "NO-COUNTEREXAMPLE", "{a} / {b}");
    }
}

#[test]
fn a_char_n_comparison_gets_no_verdict() {
    // Postgres ignores trailing blanks when comparing `character` values; a VARCHAR does not.
    let ddl = r#"create table "t" ("id" INTEGER, "c" CHAR(3), unique ("id"))"#;
    let v = label(
        r#"SELECT "id" FROM "t" WHERE "c" = 'a'"#,
        r#"SELECT "id" FROM "t" WHERE "c" = 'a  '"#,
        ddl,
    );
    assert!(v.starts_with("NOT-COMPARABLE:"), "{v}");
    // A pair that never reads the column keeps its verdict.
    assert_eq!(
        label(
            r#"SELECT "id" FROM "t""#,
            r#"SELECT "id" FROM "t" WHERE "id" > 0"#,
            ddl
        ),
        "NOT-EQUIVALENT"
    );
}

#[test]
fn numeric_arithmetic_is_exact() {
    // `x + 0.1 + 0.2 = x + 0.3` holds for every numeric x; on a DOUBLE it fails at x = 0.
    let ddl = r#"create table "t" ("id" INTEGER, "x" NUMERIC(10,2), unique ("id"))"#;
    assert_eq!(
        label(
            r#"SELECT "id" FROM "t" WHERE "x" + 0.1 + 0.2 = "x" + 0.3"#,
            r#"SELECT "id" FROM "t" WHERE "x" IS NOT NULL"#,
            ddl
        ),
        "NO-COUNTEREXAMPLE"
    );
}

#[test]
fn a_bare_numeric_cast_keeps_its_digits() {
    // DuckDB reads a bare `numeric` as DECIMAL(18,3), which rounds 0.0002 to 0.
    assert_eq!(
        label(
            r#"SELECT "id" FROM "t" WHERE CAST("a" * 0.0001 AS numeric) > 0"#,
            r#"SELECT "id" FROM "t" WHERE "a" > 0"#,
            T
        ),
        "NO-COUNTEREXAMPLE"
    );
    assert_eq!(
        label(
            r#"SELECT "id" FROM "t" WHERE ("a" * 0.0001)::numeric > 0"#,
            r#"SELECT "id" FROM "t" WHERE "a" > 0"#,
            T
        ),
        "NO-COUNTEREXAMPLE"
    );
}
