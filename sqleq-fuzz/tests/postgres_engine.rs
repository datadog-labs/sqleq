// Unless explicitly stated otherwise all files in this repository are licensed under the
// Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/).
// Copyright 2026-Present Datadog, Inc.

//! The Postgres engine (`--engine postgres`) answers as Postgres does.
//!
//! Each pair here is one the DuckDB engine refutes although Postgres says otherwise, cannot evaluate
//! at all, or needed a rewrite to evaluate; or one that pins a rule of the Postgres engine's own (how
//! a placeholder gets its type, how captured DDL is made to run). The non-equivalent pairs check that
//! the engine still refutes what it should.
//!
//! Each test starts a private cluster. Without a PostgreSQL 17 (`$SQLEQ_PG_BIN`, or `postgres` on
//! `PATH`) the tests are skipped, unless `$SQLEQ_PG_REQUIRED` is set, as CI sets it.

use sqleq_fuzz::pg::{test_pair_pg, Outcome, Server};
use sqleq_fuzz::{Config, Verdict};

/// The budget `sqleq-check` passes.
const CFG: Config = Config {
    trials: 120,
    nrows: 5,
    seed: 0,
};

fn outcome(a: &str, b: &str, ddl: &str) -> Option<Outcome> {
    let server = match Server::start(1) {
        Ok(s) => s,
        Err(e) if std::env::var_os("SQLEQ_PG_REQUIRED").is_none() => {
            eprintln!("skipped, no PostgreSQL {}: {e}", sqleq_fuzz::pg::MAJOR);
            return None;
        }
        Err(e) => panic!("{e}"),
    };
    let mut client = server.worker(0).expect("connect");
    Some(test_pair_pg(&mut client, a, b, ddl, CFG))
}

/// The part of the pair's label before its `:`, or `None` when no Postgres is there to ask.
fn kind(a: &str, b: &str, ddl: &str) -> Option<String> {
    let o = outcome(a, b, ddl)?;
    Some(
        o.verdict
            .label()
            .split(':')
            .next()
            .unwrap_or_default()
            .to_string(),
    )
}

macro_rules! assert_kind {
    ($a:expr, $b:expr, $ddl:expr, $want:expr) => {
        if let Some(got) = kind($a, $b, $ddl) {
            assert_eq!(got, $want, "\n  A: {}\n  B: {}", $a, $b);
        }
    };
}

const T: &str = r#"create table "t" ("id" INTEGER PRIMARY KEY, "a" INTEGER, "j" JSONB)"#;

// -- Constraints the DuckDB engine does not enforce --------------------------------------------

#[test]
fn a_check_constraint_holds_on_every_instance() {
    let ddl = r#"create table "t" ("id" INTEGER PRIMARY KEY, "a" INTEGER NOT NULL CHECK ("a" >= 1))"#;
    assert_kind!(
        r#"SELECT "id" FROM "t" WHERE "a" >= 1"#,
        r#"SELECT "id" FROM "t""#,
        ddl,
        "NO-COUNTEREXAMPLE"
    );
}

#[test]
fn a_foreign_key_holds_on_every_instance() {
    let ddl = r#"create table "p" ("id" INTEGER PRIMARY KEY);
create table "c" ("id" INTEGER PRIMARY KEY, "pid" INTEGER NOT NULL REFERENCES "p" ("id"))"#;
    // The join can only drop a child whose parent is missing, and the key forbids one.
    assert_kind!(
        r#"SELECT "c"."id" FROM "c" JOIN "p" ON "c"."pid" = "p"."id""#,
        r#"SELECT "id" FROM "c""#,
        ddl,
        "NO-COUNTEREXAMPLE"
    );
    // Without the key the same rewrite is refuted, by a child with no parent.
    let loose = r#"create table "p" ("id" INTEGER PRIMARY KEY);
create table "c" ("id" INTEGER PRIMARY KEY, "pid" INTEGER NOT NULL)"#;
    assert_kind!(
        r#"SELECT "c"."id" FROM "c" JOIN "p" ON "c"."pid" = "p"."id""#,
        r#"SELECT "id" FROM "c""#,
        loose,
        "NOT-EQUIVALENT"
    );
}

// -- Comparison under Postgres `=` --------------------------------------------------------------

#[test]
fn values_equal_under_equality_are_the_same_rows() {
    // 1.00 and 1.0 print differently and are one value.
    assert_kind!(
        r#"SELECT CAST("a" AS NUMERIC(10,2)) FROM "t""#,
        r#"SELECT CAST("a" AS NUMERIC(10,1)) FROM "t""#,
        T,
        "NO-COUNTEREXAMPLE"
    );
    // A numeric division keeps its scale: 1 / 2.0 is 0.50000000000000000000, and equal to 0.5.
    assert_kind!(
        r#"SELECT "a" / 2.0 FROM "t""#,
        r#"SELECT "a" * 0.5 FROM "t""#,
        T,
        "NO-COUNTEREXAMPLE"
    );
    // Their text is not one value.
    assert_kind!(
        r#"SELECT CAST(CAST("a" AS NUMERIC(10,2)) AS TEXT) FROM "t""#,
        r#"SELECT CAST(CAST("a" AS NUMERIC(10,1)) AS TEXT) FROM "t""#,
        T,
        "NOT-EQUIVALENT"
    );
}

#[test]
fn a_value_with_no_equality_is_compared_by_its_text() {
    // `json` has no `=`, and `true` and `1` are two texts.
    assert_kind!(
        r#"SELECT "id", to_json(TRUE) AS "j" FROM "t""#,
        r#"SELECT "id", to_json(1) AS "j" FROM "t""#,
        T,
        "NOT-EQUIVALENT"
    );
    // Two spellings of one document are one `jsonb` and two `json` texts.
    assert_kind!(
        r#"SELECT "id", '{"a":1}'::json AS "j" FROM "t""#,
        r#"SELECT "id", '{"a": 1}'::json AS "j" FROM "t""#,
        T,
        "NOT-EQUIVALENT"
    );
    // Every other column is still compared under `=`: 1.00 and 1.0 are one value beside equal texts.
    assert_kind!(
        r#"SELECT CAST("a" AS NUMERIC(10,2)), '{"a":1}'::json FROM "t""#,
        r#"SELECT CAST("a" AS NUMERIC(10,1)), '{"a":1}'::json FROM "t""#,
        T,
        "NO-COUNTEREXAMPLE"
    );
    // Not `json` alone: any type with no `=`, here a `point`.
    assert_kind!(
        r#"SELECT CAST("a" AS NUMERIC(10,2)), point("a", 0) FROM "t""#,
        r#"SELECT CAST("a" AS NUMERIC(10,1)), point("a", 0) FROM "t""#,
        T,
        "NO-COUNTEREXAMPLE"
    );
}

#[test]
fn an_aggregate_over_integers_is_numeric() {
    // avg of an integer is numeric, so a cast to numeric is the identity.
    assert_kind!(
        r#"SELECT avg("a") AS "m" FROM "t""#,
        r#"SELECT CAST(avg("a") AS NUMERIC) AS "m" FROM "t""#,
        T,
        "NO-COUNTEREXAMPLE"
    );
}

#[test]
fn results_of_different_shapes_differ() {
    // `*` names the columns in table order; the other side names them in another.
    let ddl = r#"create table "t" ("id" INTEGER PRIMARY KEY, "u" UUID, "j" JSONB)"#;
    assert_kind!(
        r#"SELECT * FROM "t""#,
        r#"SELECT "id", "j", "u" FROM "t""#,
        ddl,
        "NOT-EQUIVALENT"
    );
}

// -- What the DuckDB engine cannot evaluate, or evaluates only after a rewrite -------------------

#[test]
fn a_json_accessor_binds_tighter_than_and() {
    assert_kind!(
        r#"SELECT "id" FROM "t" WHERE "a" = 1 AND "j" ->> 'a' IS NULL"#,
        r#"SELECT "id" FROM "t" WHERE "j" ->> 'a' IS NULL AND "a" = 1"#,
        T,
        "NO-COUNTEREXAMPLE"
    );
}

#[test]
fn grouping_by_a_key_determines_the_other_columns() {
    assert_kind!(
        r#"SELECT "id", "a" FROM "t" GROUP BY "id""#,
        r#"SELECT "id", "a" FROM "t""#,
        T,
        "NO-COUNTEREXAMPLE"
    );
}

#[test]
fn a_semi_join_is_a_semi_join_however_it_is_spelled() {
    let ddl = r#"create table "t" ("id" INTEGER PRIMARY KEY, "a" INTEGER);
create table "u" ("id" INTEGER PRIMARY KEY, "b" INTEGER)"#;
    assert_kind!(
        r#"SELECT "id" FROM "t" WHERE "a" IN (SELECT "b" FROM "u")"#,
        r#"SELECT "id" FROM "t" WHERE EXISTS (SELECT 1 FROM "u" WHERE "u"."b" = "t"."a")"#,
        ddl,
        "NO-COUNTEREXAMPLE"
    );
}

// -- How a placeholder gets its type ------------------------------------------------------------

#[test]
fn a_placeholder_one_side_leaves_untyped_takes_the_other_sides_type() {
    // Postgres reads `SELECT $1 AS x` as text; the application bound one timestamptz to both.
    assert_kind!(
        r#"SELECT "id", $1::timestamptz AS "x" FROM "t" WHERE "id" = $2"#,
        r#"SELECT "id", $1 AS "x" FROM "t" WHERE "id" = $2"#,
        T,
        "NO-COUNTEREXAMPLE"
    );
}

#[test]
fn a_cast_to_text_is_the_sides_own_choice() {
    // '02' is 2 as an integer and not as text.
    assert_kind!(
        r#"SELECT "id" FROM "t" WHERE "id"::text = $1::text"#,
        r#"SELECT "id" FROM "t" WHERE "id" = $1"#,
        T,
        "NOT-EQUIVALENT"
    );
}

#[test]
fn a_placeholder_takes_the_type_postgres_gives_it_over_a_same_named_column() {
    // `id` is a bigint in `attachments` and a uuid in `docs`: `$1` is compared with the uuid.
    let ddl = r#"create table "attachments" ("id" BIGINT PRIMARY KEY, "doc_id" UUID NOT NULL);
create table "docs" ("id" UUID PRIMARY KEY)"#;
    assert_kind!(
        r#"SELECT a."id" FROM "attachments" a WHERE a."doc_id" IN (SELECT d."id" FROM "docs" d WHERE d."id" IN ($1))"#,
        r#"SELECT a."id" FROM "attachments" a WHERE a."doc_id" IN (SELECT DISTINCT d."id" FROM "docs" d WHERE d."id" IN ($1))"#,
        ddl,
        "NO-COUNTEREXAMPLE"
    );
}

#[test]
fn an_array_on_one_side_and_a_scalar_on_the_other_is_not_compared() {
    assert_kind!(
        r#"SELECT "id" FROM "t" WHERE "a" = $1"#,
        r#"SELECT "id" FROM "t" WHERE "a" = ANY($1::int[])"#,
        T,
        "NOT-COMPARABLE"
    );
}

#[test]
fn an_array_on_a_side_postgres_cannot_prepare_is_still_not_compared_with_a_scalar() {
    // The `unnest` side assigns text to a uuid column, so Postgres types none of its placeholders;
    // its own casts still say they are arrays. Its `WHERE` makes it no pair of the gather rule's
    // shape, which would be run under that rule instead (below).
    let ddl = r#"create table "s" ("id" UUID PRIMARY KEY, "name" TEXT)"#;
    assert_kind!(
        r#"INSERT INTO "s" ("id", "name") VALUES (CAST($1 AS UUID), $2)"#,
        r#"INSERT INTO "s" ("id", "name") SELECT * FROM unnest($1::text[], $2::pg_catalog.text[]) WHERE true"#,
        ddl,
        "NOT-COMPARABLE"
    );
}

#[test]
fn a_placeholder_where_no_parameter_may_stand_is_a_literal() {
    // `interval $1` is a syntax error with a parameter; captured SQL writes a literal's place so.
    let ddl = r#"create table "t" ("id" INTEGER PRIMARY KEY, "ts" TIMESTAMP)"#;
    assert_kind!(
        r#"SELECT "id" FROM "t" WHERE "ts" <= now() - interval $1"#,
        r#"SELECT "id" FROM "t" WHERE "ts" <= now() - interval $1 AND "id" IS NOT NULL"#,
        ddl,
        "NO-COUNTEREXAMPLE"
    );
}

// -- Captured DDL made to run -------------------------------------------------------------------

#[test]
fn a_table_is_found_under_the_schema_the_queries_name() {
    // The DDL comes from one schema and the queries from another of the same layout.
    let ddl = r#"CREATE TABLE t (id integer PRIMARY KEY, a integer);
CREATE INDEX t_a ON east.t USING btree (a);"#;
    assert_kind!(
        "SELECT a FROM west.t WHERE id = 1",
        "SELECT a FROM west.t WHERE id = 1 AND a IS NOT NULL",
        ddl,
        "NOT-EQUIVALENT"
    );
}

#[test]
fn an_undeclared_type_is_read_as_text_and_said_so() {
    let ddl = r#"CREATE TABLE t (id integer PRIMARY KEY, st status_kind NOT NULL)"#;
    let Some(o) = outcome(
        "SELECT id FROM t WHERE st = 'a'",
        "SELECT id FROM t WHERE st IN ('a')",
        ddl,
    ) else {
        return;
    };
    assert_eq!(o.verdict.label(), "NO-COUNTEREXAMPLE");
    assert_eq!(o.timing.stand_ins, 1);
    assert!(o.timing.caveat().unwrap().contains("read as text"));
}

#[test]
fn a_default_calling_an_undeclared_function_is_dropped_and_said_so() {
    let ddl = r#"CREATE TABLE t (id uuid PRIMARY KEY DEFAULT gen_id(), a integer)"#;
    let Some(o) = outcome(
        "SELECT a FROM t WHERE a > 0",
        "SELECT a FROM t WHERE a >= 1",
        ddl,
    ) else {
        return;
    };
    assert_eq!(o.verdict.label(), "NO-COUNTEREXAMPLE");
    assert_eq!(o.timing.dropped_defaults, 1);
}

// -- Mutations ----------------------------------------------------------------------------------

#[test]
fn each_side_draws_the_same_sequence_values() {
    // Without a reset before each side, the second insert would take the next id.
    let ddl = r#"create table "t" ("id" SERIAL PRIMARY KEY, "a" INTEGER)"#;
    assert_kind!(
        r#"INSERT INTO "t" ("a") VALUES (7)"#,
        r#"INSERT INTO "t" ("a") SELECT 7"#,
        ddl,
        "NO-COUNTEREXAMPLE"
    );
}

#[test]
fn a_volatile_default_draws_a_value_of_its_own_on_each_side() {
    // Each side's insert draws its own uuid for `id`, so the two tables differ in it whatever the
    // statements are; only the number of rows is a fact about them.
    let ddl = r#"create table "t" ("id" UUID PRIMARY KEY DEFAULT gen_random_uuid(), "a" INTEGER)"#;
    assert_kind!(
        r#"INSERT INTO "t" ("a") VALUES (7)"#,
        r#"INSERT INTO "t" ("a") SELECT 7"#,
        ddl,
        "NO-COUNTEREXAMPLE"
    );
    assert_kind!(
        r#"INSERT INTO "t" ("a") VALUES (7)"#,
        r#"INSERT INTO "t" ("a") VALUES (7), (8)"#,
        ddl,
        "NOT-EQUIVALENT"
    );
}

#[test]
fn a_mutation_that_touches_other_rows_is_refuted() {
    assert_kind!(
        r#"UPDATE "t" SET "a" = "a" + 1 WHERE "id" = $1"#,
        r#"UPDATE "t" SET "a" = "a" + 1"#,
        T,
        "NOT-EQUIVALENT"
    );
}

// -- The clock ----------------------------------------------------------------------------------

const EVENTS: &str = r#"create table "e" ("id" INTEGER PRIMARY KEY, "at" TIMESTAMPTZ NOT NULL)"#;

#[test]
fn a_boundary_at_the_clock_is_refuted() {
    // The middle clock is a generated instant, so a row stamped with it tells `<` from `<=`.
    assert_kind!(
        r#"SELECT "id" FROM "e" WHERE "at" < now()"#,
        r#"SELECT "id" FROM "e" WHERE "at" <= now()"#,
        EVENTS,
        "NOT-EQUIVALENT"
    );
}

#[test]
fn a_comparison_with_the_clock_that_one_side_drops_is_refuted() {
    // Against a clock after every generated row the comparison holds for all of them; against one
    // before them, for none. The counterexample names the clock it was found under.
    if let Some(o) = outcome(
        r#"SELECT "id" FROM "e" WHERE "at" < current_timestamp - interval '1 day'"#,
        r#"SELECT "id" FROM "e""#,
        EVENTS,
    ) {
        match o.verdict {
            Verdict::NotEquivalent(witness) => assert!(witness.contains("clock: "), "{witness}"),
            v => panic!("{}", v.label()),
        }
    }
}

#[test]
fn every_read_of_the_clock_in_a_trial_reads_one_instant() {
    // A default and a statement's own read; `now()`, `current_timestamp` and an old dump's
    // `('now'::text)::date` against `current_date`.
    let ddl = r#"create table "e" ("id" INTEGER PRIMARY KEY, "at" TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
  "day" DATE DEFAULT ('now'::text)::date)"#;
    assert_kind!(
        r#"INSERT INTO "e" ("id", "at", "day") VALUES ($1, now(), current_date)"#,
        r#"INSERT INTO "e" ("id") VALUES ($1)"#,
        ddl,
        "NO-COUNTEREXAMPLE"
    );
    assert_kind!(
        r#"SELECT "id", now()::time AS "x", now()::date AS "d" FROM "e""#,
        r#"SELECT "id", localtime AS "x", pg_catalog.now()::date AS "d" FROM "e""#,
        EVENTS,
        "NO-COUNTEREXAMPLE"
    );
}

#[test]
fn a_function_body_that_reads_the_clock_is_not_compared() {
    // The trigger would stamp the row with the real clock, which no statement's own read sees.
    let ddl = r#"create table "e" ("id" INTEGER PRIMARY KEY, "at" TIMESTAMPTZ);
create function "stamp"() returns trigger language plpgsql as $$ begin new."at" := now(); return new; end $$;
create trigger "e_stamp" before update on "e" for each row execute function "stamp"()"#;
    assert_kind!(
        r#"UPDATE "e" SET "at" = now() WHERE "id" = $1"#,
        r#"UPDATE "e" SET "id" = "id" WHERE "id" = $1"#,
        ddl,
        "NOT-COMPARABLE"
    );
}

// -- The gather rule: `INSERT … VALUES` against `INSERT … SELECT * FROM unnest(…)` ----------------
//
// Each class of mismatch is refuted, beside a control the same but for the mismatch.

const VALUES2: &str = r#"INSERT INTO "s" ("id", "name") VALUES ($1, $2), ($3, $4)"#;

/// `VALUES2` against `unnest` of these two arrays.
fn unnest2(id: &str, name: &str) -> String {
    format!(r#"INSERT INTO "s" ("id", "name") SELECT * FROM unnest($1::{id}[], $2::{name}[])"#)
}

/// The counterexample, or `None` when no Postgres is there to ask.
fn witness(a: &str, b: &str, ddl: &str) -> Option<String> {
    match outcome(a, b, ddl)?.verdict {
        Verdict::NotEquivalentGather(w) => Some(w),
        v => panic!("not a gather counterexample: {}\n  A: {a}\n  B: {b}", v.label()),
    }
}

#[test]
fn text_elements_into_a_uuid_column_are_refuted() {
    let ddl = r#"create table "s" ("id" UUID PRIMARY KEY, "name" TEXT)"#;
    assert_kind!(VALUES2, &unnest2("text", "text"), ddl, "NOT-EQUIVALENT-GATHER");
    assert_kind!(VALUES2, &unnest2("uuid", "text"), ddl, "NO-COUNTEREXAMPLE-GATHER");
    // Which side comes first does not matter.
    assert_kind!(&unnest2("text", "text"), VALUES2, ddl, "NOT-EQUIVALENT-GATHER");
    if let Some(w) = witness(VALUES2, &unnest2("text", "text"), ddl) {
        assert!(w.contains("unnest side: $1='{"), "{w}");
        assert!(w.contains("B raises 42804"), "{w}");
    }
}

#[test]
fn elements_narrower_than_the_column_are_refuted_past_their_range() {
    let ddl = r#"create table "s" ("id" BIGINT, "name" TEXT)"#;
    assert_kind!(VALUES2, &unnest2("int", "text"), ddl, "NOT-EQUIVALENT-GATHER");
    assert_kind!(VALUES2, &unnest2("bigint", "text"), ddl, "NO-COUNTEREXAMPLE-GATHER");
}

#[test]
fn an_element_cast_that_truncates_is_refuted() {
    // A bare `char` is `char(1)`: the explicit cast keeps one character, the column all of them.
    let ddl = r#"create table "s" ("id" INTEGER, "name" TEXT)"#;
    assert_kind!(VALUES2, &unnest2("int", "char"), ddl, "NOT-EQUIVALENT-GATHER");
    assert_kind!(VALUES2, &unnest2("int", "text"), ddl, "NO-COUNTEREXAMPLE-GATHER");
    // Into a `varchar(8)` column the `VALUES` side raises on a long value, where the cast to
    // `varchar(8)` truncates it; with `text` elements both sides raise.
    let ddl = r#"create table "s" ("id" INTEGER, "name" VARCHAR(8))"#;
    assert_kind!(VALUES2, &unnest2("int", "varchar(8)"), ddl, "NOT-EQUIVALENT-GATHER");
    assert_kind!(VALUES2, &unnest2("int", "text"), ddl, "NO-COUNTEREXAMPLE-GATHER");
}

#[test]
fn an_element_cast_that_rounds_is_refuted() {
    let ddl = r#"create table "s" ("id" NUMERIC(10, 2), "name" TEXT)"#;
    assert_kind!(VALUES2, &unnest2("numeric(10, 1)", "text"), ddl, "NOT-EQUIVALENT-GATHER");
    // The same modifier rounds as the assignment does, and none leaves it to the assignment.
    assert_kind!(VALUES2, &unnest2("numeric(10, 2)", "text"), ddl, "NO-COUNTEREXAMPLE-GATHER");
    assert_kind!(VALUES2, &unnest2("numeric", "text"), ddl, "NO-COUNTEREXAMPLE-GATHER");
}

#[test]
fn timestamptz_elements_into_a_timestamp_column_are_refuted_where_the_clocks_change() {
    // A local time that does not exist in New York comes back an hour later from `timestamptz`.
    let ddl = r#"create table "s" ("id" TIMESTAMP, "name" TEXT)"#;
    assert_kind!(VALUES2, &unnest2("timestamp", "text"), ddl, "NO-COUNTEREXAMPLE-GATHER");
    if let Some(w) = witness(VALUES2, &unnest2("timestamptz", "text"), ddl) {
        assert!(w.contains("TimeZone: America/New_York"), "{w}");
    }
}

#[test]
fn an_array_column_is_refuted() {
    // `unnest` flattens, so a `text[]` element is a `text`, and the column wants a `text[]`.
    let ddl = r#"create table "s" ("id" INTEGER, "name" TEXT[])"#;
    assert_kind!(VALUES2, &unnest2("int", "text"), ddl, "NOT-EQUIVALENT-GATHER");
}

#[test]
fn the_arrays_follow_the_values_sides_columns() {
    let ddl = r#"create table "s" ("id" INTEGER PRIMARY KEY, "name" TEXT)"#;
    // Listed the other way round on both sides, the same pair.
    let tail = r#"ON CONFLICT ("id") DO NOTHING RETURNING "id""#;
    assert_kind!(
        &format!(r#"INSERT INTO "s" ("name", "id") VALUES ($1, $2), ($3, $4) {tail}"#),
        &format!(r#"INSERT INTO "s" ("name", "id") SELECT * FROM unnest($1::text[], $2::int[]) {tail}"#),
        ddl,
        "NO-COUNTEREXAMPLE-GATHER"
    );
    // The arrays the other way round: `$1` is the `id` column, whatever argument it is.
    let ddl = r#"create table "s" ("id" TEXT, "name" TEXT)"#;
    let swapped = r#"INSERT INTO "s" ("id", "name") SELECT * FROM unnest($2::text[], $1::text[])"#;
    assert_kind!(VALUES2, swapped, ddl, "NOT-EQUIVALENT-GATHER");
}

#[test]
fn a_one_sided_error_counts_only_when_the_statements_alone_explain_it() {
    // The same text-into-uuid pair, but a trigger could raise on either side's rows, and an
    // undeclared type is a text domain here: no trial is a counterexample.
    let trigger = r#"create table "s" ("id" UUID PRIMARY KEY, "name" TEXT);
create function "f"() returns trigger language plpgsql as $$ begin return new; end $$;
create trigger "s_f" before insert on "s" for each row execute function "f"()"#;
    assert_kind!(VALUES2, &unnest2("text", "text"), trigger, "ERROR");
    let repaired = r#"create table "s" ("id" UUID PRIMARY KEY, "name" "status")"#;
    assert_kind!(VALUES2, &unnest2("text", "status"), repaired, "ERROR");
    // A `VALUES` side Postgres cannot prepare as written: how a client typed its parameters could
    // change whether it runs, so its raising is not counted either, though here it always would.
    let ddl = r#"create table "s" ("id" INTEGER, "name" TEXT)"#;
    assert_kind!(
        r#"INSERT INTO "s" ("id", "name") VALUES ($1::text, $2), ($3::text, $4)"#,
        &unnest2("int", "text"),
        ddl,
        "ERROR"
    );
}

#[test]
fn a_gather_pair_the_rule_cannot_bind_is_not_compared() {
    let ddl = r#"create table "s" ("id" SERIAL PRIMARY KEY, "name" TEXT)"#;
    assert_kind!(
        r#"INSERT INTO "s" ("id", "name") VALUES (DEFAULT, $1), (DEFAULT, $2)"#,
        &unnest2("int", "text"),
        ddl,
        "NOT-COMPARABLE"
    );
}

// -- Rows for the tables the DDL leaves (issue #143) ---------------------------------------------
//
// The rows are generated for each table as Postgres's catalog holds it once the DDL has run: its
// own columns, each NOT NULL exactly where Postgres says, and every table inheriting from one the
// queries read.

#[test]
fn a_dropped_not_null_admits_a_null() {
    let a = r#"SELECT "a" FROM "t" WHERE "id" IS NOT NULL"#;
    let b = r#"SELECT "a" FROM "t""#;
    let ddl = r#"create table "t" ("id" INTEGER NOT NULL, "a" INTEGER);
                 alter table "t" alter column "id" drop not null"#;
    assert_kind!(a, b, ddl, "NOT-EQUIVALENT");
    // Wherever the DDL drops it: the catalog is read, not the DDL's text.
    let ddl = r#"create table "t" ("id" INTEGER NOT NULL, "a" INTEGER);
                 do $$ begin alter table "t" alter column "id" drop not null; end $$"#;
    assert_kind!(a, b, ddl, "NOT-EQUIVALENT");
    // A column still NOT NULL gets no NULL.
    let ddl = r#"create table "t" ("id" INTEGER NOT NULL, "a" INTEGER NOT NULL);
                 alter table "t" alter column "a" drop not null"#;
    assert_kind!(a, b, ddl, "NO-COUNTEREXAMPLE");
}

#[test]
fn a_renamed_column_is_filled_under_its_new_name() {
    // Two renames move the NOT NULL from the name `a` to the name `c`.
    let ddl = r#"create table "t" ("a" INTEGER NOT NULL, "b" INTEGER);
                 alter table "t" rename column "a" to "c";
                 alter table "t" rename column "b" to "a""#;
    assert_kind!(
        r#"SELECT "a" FROM "t" WHERE "a" IS NOT NULL"#,
        r#"SELECT "a" FROM "t""#,
        ddl,
        "NOT-EQUIVALENT"
    );
    // Three swap the two, and `b` is the NOT NULL one.
    let ddl = r#"create table "t" ("a" INTEGER NOT NULL, "b" INTEGER);
                 alter table "t" rename column "a" to "c";
                 alter table "t" rename column "b" to "a";
                 alter table "t" rename column "c" to "b""#;
    assert_kind!(
        r#"SELECT "b" FROM "t" WHERE "b" IS NOT NULL"#,
        r#"SELECT "b" FROM "t""#,
        ddl,
        "NO-COUNTEREXAMPLE"
    );
}

#[test]
fn a_table_inheriting_from_one_the_queries_read_has_rows() {
    let ddl = r#"create table "p" ("id" INTEGER PRIMARY KEY);
                 create table "c" ("x" INTEGER) inherits ("p")"#;
    assert_kind!(
        r#"SELECT DISTINCT "id" FROM "p""#,
        r#"SELECT "id" FROM "p""#,
        ddl,
        "NOT-EQUIVALENT"
    );
    // `ONLY` reads the parent alone, which its key binds.
    assert_kind!(
        r#"SELECT DISTINCT "id" FROM ONLY "p""#,
        r#"SELECT "id" FROM ONLY "p""#,
        ddl,
        "NO-COUNTEREXAMPLE"
    );
    // The rows reach every generation, and the counterexample shows them.
    let ddl = r#"create table "p" ("id" INTEGER PRIMARY KEY);
                 create table "c" () inherits ("p");
                 create table "g" ("y" INTEGER) inherits ("c")"#;
    if let Some(o) = outcome(r#"SELECT DISTINCT "id" FROM "p""#, r#"SELECT "id" FROM "p""#, ddl) {
        match o.verdict {
            Verdict::NotEquivalent(w) => assert!(w.contains("g=["), "{w}"),
            v => panic!("{}", v.label()),
        }
    }
}

#[test]
fn a_table_created_like_another_has_its_columns() {
    let a = r#"SELECT DISTINCT "id" FROM "t""#;
    let b = r#"SELECT "id" FROM "t""#;
    let ddl = r#"create table "o" ("id" INTEGER PRIMARY KEY);
                 create table "t" (like "o")"#;
    assert_kind!(a, b, ddl, "NOT-EQUIVALENT");
    // With its indexes, it has the key too.
    let ddl = r#"create table "o" ("id" INTEGER PRIMARY KEY);
                 create table "t" (like "o" including indexes)"#;
    assert_kind!(a, b, ddl, "NO-COUNTEREXAMPLE");
}

#[test]
fn the_gather_rule_loads_rows_into_a_renamed_column() {
    // Only a row already in the table, with the `id` the `VALUES` row brings, tells `DO UPDATE`
    // from
    // `DO NOTHING`; under its old name `id` was left out of the rows loaded, and every one of them
    // was refused.
    let ddl = r#"create table "s" ("k" INTEGER PRIMARY KEY, "name" TEXT);
                 alter table "s" rename column "k" to "id""#;
    assert_kind!(
        r#"INSERT INTO "s" ("id", "name") VALUES ($1, $2) ON CONFLICT ("id") DO UPDATE SET "name" = excluded."name""#,
        r#"INSERT INTO "s" ("id", "name") SELECT * FROM unnest($1::int[], $2::text[]) ON CONFLICT ("id") DO NOTHING"#,
        ddl,
        "NOT-EQUIVALENT-GATHER"
    );
}

// -- Plain refutations still found --------------------------------------------------------------

#[test]
fn a_changed_predicate_is_refuted() {
    assert_kind!(
        r#"SELECT "id" FROM "t" WHERE "a" > 0"#,
        r#"SELECT "id" FROM "t" WHERE "a" >= 0"#,
        T,
        "NOT-EQUIVALENT"
    );
}
