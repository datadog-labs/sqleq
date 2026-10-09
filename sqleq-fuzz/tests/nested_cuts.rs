// Unless explicitly stated otherwise all files in this repository are licensed under the
// Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/).
// Copyright 2026-Present Datadog, Inc.

//! A cut whose ordering leaves ties, under a level that can tell the tied rows apart (issue #124).
//!
//! Comparing such a trial by cardinality is sound only where the cardinality does not depend on
//! which tied rows the cut kept. Under a level that filters, joins or tests membership in them it
//! does: two equivalent sides that read the same rows in different physical orders -- one through
//! a join, a `DISTINCT` or a `UNION ALL` -- keep different tied rows, and the level above lets a
//! different number of them through. The equivalent pairs here were refuted before the fix; the
//! others check that what can still be compared still is.
//!
//! The Postgres tests start a private cluster each. Without a PostgreSQL 17 (`$SQLEQ_PG_BIN`, or
//! `postgres` on `PATH`) they are skipped, unless `$SQLEQ_PG_REQUIRED` is set, as CI sets it.

use sqleq_fuzz::pg::{test_pair_pg, Server};
use sqleq_fuzz::{test_pair, Config};

/// The budget `sqleq-check` passes.
const CFG: Config = Config {
    trials: 120,
    nrows: 5,
    seed: 0,
};

/// The part of a label before its `:`.
fn kind(label: String) -> String {
    label.split(':').next().unwrap_or_default().to_string()
}

/// The pair's verdict on the Postgres engine, or `None` when no Postgres is there to ask.
fn pg(a: &str, b: &str, ddl: &str, cfg: Config) -> Option<String> {
    let server = match Server::start(1) {
        Ok(s) => s,
        Err(e) if std::env::var_os("SQLEQ_PG_REQUIRED").is_none() => {
            eprintln!("skipped, no PostgreSQL {}: {e}", sqleq_fuzz::pg::MAJOR);
            return None;
        }
        Err(e) => panic!("{e}"),
    };
    let mut client = server.worker(0).expect("connect");
    Some(kind(test_pair_pg(&mut client, a, b, ddl, cfg).verdict.label()))
}

/// The pair's verdict on the DuckDB engine.
fn duck(a: &str, b: &str, ddl: &str, cfg: Config) -> String {
    kind(test_pair(a, b, ddl, cfg).label())
}

macro_rules! assert_pg {
    ($a:expr, $b:expr, $ddl:expr, $want:expr) => {
        assert_pg!($a, $b, $ddl, $want, CFG)
    };
    ($a:expr, $b:expr, $ddl:expr, $want:expr, $cfg:expr) => {
        if let Some(got) = pg($a, $b, $ddl, $cfg) {
            assert_eq!(got, $want, "\n  A: {}\n  B: {}", $a, $b);
        }
    };
}

const T: &str = r#"create table "t" ("id" INTEGER PRIMARY KEY, "g" INTEGER, "v" INTEGER)"#;

/// The cut every pair here takes: the first row by `g`, which ties.
const A: &str = r#"SELECT "id" FROM (SELECT * FROM "t" ORDER BY "g" LIMIT 1) AS "s" WHERE "v" = 1"#;

// -- Equivalent pairs that were refuted --------------------------------------------------------

#[test]
fn a_redundant_distinct_under_a_filtered_cut_is_not_refuted() {
    // `id` is a key, so `DISTINCT *` keeps every row of `t`; Postgres hashes them into another
    // order, and keeps another of the rows tied on `g`.
    assert_pg!(
        A,
        r#"SELECT "id" FROM (SELECT DISTINCT * FROM "t" ORDER BY "g" LIMIT 1) AS "s" WHERE "v" = 1"#,
        T,
        "NONDET-SKIP"
    );
}

#[test]
fn a_join_every_row_survives_under_a_filtered_cut_is_not_refuted() {
    // Every row of `t` references one row of `k`, so the join and the semi-join keep `t` whole;
    // a hash join hands the rows on in `k`'s order.
    let ddl = r#"create table "k" ("id" INTEGER PRIMARY KEY);
                 create table "t" ("id" INTEGER PRIMARY KEY REFERENCES "k", "g" INTEGER, "v" INTEGER)"#;
    assert_pg!(
        A,
        r#"SELECT "id" FROM (SELECT "t".* FROM "t" JOIN "k" ON "k"."id" = "t"."id" ORDER BY "g" LIMIT 1) AS "s" WHERE "v" = 1"#,
        ddl,
        "NONDET-SKIP"
    );
    assert_pg!(
        A,
        r#"SELECT "id" FROM (SELECT * FROM "t" WHERE "id" IN (SELECT "id" FROM "k") ORDER BY "g" LIMIT 1) AS "s" WHERE "v" = 1"#,
        ddl,
        "NONDET-SKIP"
    );
}

/// `t` as the rows with `v = 1` followed by the others: every row once, in another order.
const PARTS: &str = r#"(SELECT * FROM "t" WHERE "v" = 1 UNION ALL SELECT * FROM "t" WHERE "v" <> 1 OR "v" IS NULL)"#;

#[test]
fn a_partition_under_a_filtered_or_tested_cut_is_not_refuted_on_either_engine() {
    let b = format!(
        r#"SELECT "id" FROM (SELECT * FROM {PARTS} AS "x" ORDER BY "g" LIMIT 1) AS "s" WHERE "v" = 1"#
    );
    assert_pg!(A, &b, T, "NONDET-SKIP");
    assert_eq!(duck(A, &b, T, CFG), "NONDET-SKIP");
    let a = r#"SELECT "id" FROM "t" WHERE "id" IN (SELECT "id" FROM "t" ORDER BY "g" LIMIT 2) AND "v" = 1"#;
    let b = format!(
        r#"SELECT "id" FROM "t" WHERE "id" IN (SELECT "id" FROM {PARTS} AS "x" ORDER BY "g" LIMIT 2) AND "v" = 1"#
    );
    assert_pg!(a, &b, T, "NONDET-SKIP");
    assert_eq!(duck(a, &b, T, CFG), "NONDET-SKIP");
}

#[test]
fn a_parameter_count_bound_to_cut_under_a_filter_compares_nothing() {
    // `LIMIT $1` cuts nothing in most trials, which compare whole bags; in the small trials that
    // bind it so that it cuts, the filter above sees the choice, so nothing is compared there.
    // Seed 2 is one on which those trials kept different rows.
    let a = r#"SELECT "id" FROM (SELECT * FROM "t" ORDER BY "g" LIMIT $1) AS "s" WHERE "v" = 1"#;
    let b = format!(
        r#"SELECT "id" FROM (SELECT * FROM {PARTS} AS "x" ORDER BY "g" LIMIT $1) AS "s" WHERE "v" = 1"#
    );
    let cfg = Config { seed: 2, ..CFG };
    assert_pg!(a, &b, T, "NO-COUNTEREXAMPLE", cfg);
    assert_eq!(duck(a, &b, T, cfg), "NO-COUNTEREXAMPLE");
}

// -- What can still be compared ----------------------------------------------------------------

#[test]
fn a_cut_at_the_top_is_still_compared_by_cardinality() {
    assert_pg!(
        r#"SELECT "id" FROM "t" ORDER BY "g" LIMIT 1"#,
        r#"SELECT "id" FROM "t" ORDER BY "g" LIMIT 2"#,
        T,
        "NOT-EQUIVALENT"
    );
    assert_pg!(
        r#"(SELECT "id" FROM "t" ORDER BY "g" LIMIT 1) UNION ALL (SELECT "id" FROM "t" ORDER BY "g" LIMIT 1)"#,
        r#"SELECT "id" FROM "t" ORDER BY "g" LIMIT 2"#,
        T,
        "NOT-EQUIVALENT"
    );
    // Under a select list, which keeps each of its rows.
    assert_pg!(
        r#"SELECT "v" FROM (SELECT * FROM "t" ORDER BY "g" LIMIT 1) AS "s""#,
        r#"SELECT "v" FROM (SELECT * FROM "t" ORDER BY "g" LIMIT 2) AS "s""#,
        T,
        "NOT-EQUIVALENT"
    );
}

#[test]
fn a_cut_a_join_reads_nothing_of_is_still_compared_by_cardinality() {
    // Each row of `u` is paired with as many rows as the cut keeps for it, whichever they are.
    let ddl = r#"create table "t" ("id" INTEGER PRIMARY KEY, "g" INTEGER, "v" INTEGER);
                 create table "u" ("id" INTEGER PRIMARY KEY, "x" INTEGER)"#;
    assert_pg!(
        r#"SELECT "u"."id", "l"."v" FROM "u" CROSS JOIN LATERAL (SELECT "v" FROM "t" WHERE "t"."g" = "u"."x" ORDER BY "v" LIMIT 1) AS "l""#,
        r#"SELECT "u"."id", "l"."v" FROM "u" CROSS JOIN LATERAL (SELECT "v" FROM "t" WHERE "t"."g" = "u"."x" ORDER BY "v" LIMIT 2) AS "l""#,
        ddl,
        "NOT-EQUIVALENT"
    );
}

#[test]
fn a_cut_under_exists_is_still_compared() {
    // `EXISTS` reads only whether there is a row, which no tie-break changes.
    let ddl = r#"create table "t" ("id" INTEGER PRIMARY KEY, "g" INTEGER, "v" INTEGER);
                 create table "u" ("id" INTEGER PRIMARY KEY, "x" INTEGER)"#;
    assert_pg!(
        r#"SELECT "id" FROM "u" WHERE EXISTS (SELECT 1 FROM "t" WHERE "t"."g" = "u"."x" ORDER BY "v" LIMIT 1)"#,
        r#"SELECT "id" FROM "u""#,
        ddl,
        "NOT-EQUIVALENT"
    );
}

#[test]
fn a_total_cut_under_a_filter_is_still_compared_whole() {
    assert_pg!(
        r#"SELECT "id" FROM (SELECT * FROM "t" ORDER BY "id" LIMIT 2) AS "s" WHERE "v" = 1"#,
        r#"SELECT "id" FROM (SELECT * FROM "t" ORDER BY "id" LIMIT 2) AS "s" WHERE "v" = 2"#,
        T,
        "NOT-EQUIVALENT"
    );
}
