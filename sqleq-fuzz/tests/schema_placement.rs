// Unless explicitly stated otherwise all files in this repository are licensed under the
// Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/).
// Copyright 2026-Present Datadog, Inc.

//! A DDL that declares one table name both bare and schema-qualified (issue #125).
//!
//! Captured DDL often drops the schema from a `CREATE TABLE` but keeps it on the table's indexes, so
//! the Postgres engine creates a bare table in the one schema the rest of the DDL names it by. A
//! `CREATE TABLE s.t` beside the bare `t` is not such a name: it is another table, and the bare `t`
//! is `public.t`, as Postgres places it with no search path declared. Moving it into `s` made the
//! DDL fail with `relation "t" already exists`, so every pair here was an error before the fix.
//!
//! Each test starts a private cluster. Without a PostgreSQL 17 (`$SQLEQ_PG_BIN`, or `postgres` on
//! `PATH`) the tests are skipped, unless `$SQLEQ_PG_REQUIRED` is set, as CI sets it.

use sqleq_fuzz::pg::{test_pair_pg, Server};
use sqleq_fuzz::Config;

/// The budget `sqleq-check` passes.
const CFG: Config = Config {
    trials: 120,
    nrows: 5,
    seed: 0,
};

/// The part of the pair's label before its `:`, or `None` when no Postgres is there to ask.
fn kind(a: &str, b: &str, ddl: &str) -> Option<String> {
    let server = match Server::start(1) {
        Ok(s) => s,
        Err(e) if std::env::var_os("SQLEQ_PG_REQUIRED").is_none() => {
            eprintln!("skipped, no PostgreSQL {}: {e}", sqleq_fuzz::pg::MAJOR);
            return None;
        }
        Err(e) => panic!("{e}"),
    };
    let mut client = server.worker(0).expect("connect");
    let label = test_pair_pg(&mut client, a, b, ddl, CFG).verdict.label();
    Some(label.split(':').next().unwrap_or_default().to_string())
}

macro_rules! assert_kind {
    ($a:expr, $b:expr, $ddl:expr, $want:expr) => {
        if let Some(got) = kind($a, $b, $ddl) {
            assert_eq!(got, $want, "\n  A: {}\n  B: {}\n  DDL: {}", $a, $b, $ddl);
        }
    };
}

/// A bare `t` and an `s.t` of the same layout.
const BOTH: &str = r#"create table "t" ("a" INTEGER);
create table "s"."t" ("a" INTEGER);"#;

#[test]
fn a_bare_table_and_a_qualified_one_of_its_name_are_two_tables() {
    // The issue's pair: `t = {(1)}`, `s.t = {}` give 1 against no rows.
    assert_kind!(r#"SELECT "a" FROM "t""#, r#"SELECT "a" FROM "s"."t""#, BOTH, "NOT-EQUIVALENT");
    // An index on `s.t` names the schema the DDL creates `s.t` in, and moves nothing either.
    let indexed = format!("{BOTH}\ncreate index \"i\" on \"s\".\"t\" (\"a\");");
    assert_kind!(
        r#"SELECT "a" FROM "t""#,
        r#"SELECT "a" FROM "s"."t""#,
        indexed.as_str(),
        "NOT-EQUIVALENT"
    );
}

#[test]
fn the_bare_table_is_the_one_in_public() {
    assert_kind!(
        r#"SELECT "a" FROM "t""#,
        r#"SELECT "a" FROM "public"."t""#,
        BOTH,
        "NO-COUNTEREXAMPLE"
    );
}

#[test]
fn the_qualified_table_is_the_one_in_its_schema() {
    assert_kind!(
        r#"SELECT "a" FROM "s"."t""#,
        r#"SELECT "a" FROM s.t"#,
        BOTH,
        "NO-COUNTEREXAMPLE"
    );
}
