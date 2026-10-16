// Unless explicitly stated otherwise all files in this repository are licensed under the
// Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/).
// Copyright 2026-Present Datadog, Inc.

//! A refusal's kind says what Postgres makes of the input (see `FrontendError`): `parse` when its
//! grammar rejects the text, `schema` when Postgres parses it and rejects it, `unsupported` when
//! Postgres accepts it and the frontend declines, its own parser included. Each pair
//! below was run through Postgres 17, and what it said about side A is written beside the pair: its
//! error, or that it prepares. A tie-break refusal is `unsupported` although the input shown fails in
//! Postgres, because the same refusal also meets input Postgres accepts and the syntax in front of
//! the frontend cannot tell the two apart. The controls are the valid neighbours, which still lower.

use sqleq_frontend::{lower_with, CatalogSource, FrontendError};

const T: &str = r#"create table "t" ("a" INTEGER, "b" INTEGER, unique ("a"));
create table "u" ("a" INTEGER, "c" INTEGER);"#;

const BOTH: [CatalogSource; 2] = [CatalogSource::Declared, CatalogSource::InferredSeeded];

#[derive(Clone, Copy, Debug)]
enum Kind {
    Parse,
    Schema,
    Unsupported,
}

/// A refused pair: its two sides, the kind and a fragment of the reason, and Postgres's verdict on
/// side A.
struct Row {
    kind: Kind,
    a: &'static str,
    b: &'static str,
    reason: &'static str,
    postgres: &'static str,
}

const fn row(kind: Kind, a: &'static str, b: &'static str, reason: &'static str, postgres: &'static str) -> Row {
    Row { kind, a, b, reason, postgres }
}

use Kind::{Parse, Schema, Unsupported};

/// Text Postgres's grammar rejects, whatever sqlparser made of it.
const NOT_POSTGRES: &[Row] = &[
    row(Parse, r#"SELECT TOP 1 "a" FROM "t""#, r#"SELECT "a" FROM "t" LIMIT 1"#, "syntax error",
        r#"42601 syntax error at or near "1""#),
    row(Parse, r#"SELECT TRY_CAST("b" AS TEXT) FROM "t""#, r#"SELECT CAST("b" AS TEXT) FROM "t""#, "syntax error",
        r#"42601 syntax error at or near "AS""#),
    row(Parse, r#"SELECT "a" FROM "t" QUALIFY "a" > 1"#, r#"SELECT "a" FROM "t""#, "syntax error",
        r#"42601 syntax error at or near "a", with QUALIFY read as the table's alias"#),
    row(Parse, r#"SELECT 1x FROM "t""#, r#"SELECT 1 FROM "t""#, "trailing junk",
        r#"42601 trailing junk after numeric literal at or near "1x""#),
    // Two clauses sqlparser accepts and the lowering used to drop, so that each side lowered like the
    // other: Postgres's grammar has neither.
    row(Parse, r#"SELECT CAST("a" AS TEXT FORMAT 'YYYY') FROM "t""#, r#"SELECT CAST("a" AS TEXT) FROM "t""#,
        "syntax error", r#"42601 syntax error at or near "FORMAT""#),
    row(Parse, r#"SELECT "a", count(*) FROM "t" GROUP BY "a" GROUPING SETS (("a"), ())"#,
        r#"SELECT "a", count(*) FROM "t" GROUP BY "a""#, "syntax error",
        r#"42601 syntax error at or near "GROUPING""#),
];

/// Input Postgres rejects.
const REJECTED: &[Row] = &[
    row(Schema, r#"UPDATE "t" SET "b" = 1, "b" = 2"#, r#"UPDATE "t" SET "b" = 2"#, "SET assigns b twice",
        "42601 multiple assignments to same column"),
    row(Schema, r#"INSERT INTO "t" ("a", "a") VALUES (1, 2)"#, r#"INSERT INTO "t" ("a") VALUES (2)"#,
        "INSERT lists a twice", "42701 column specified more than once"),
    row(Schema, r#"DELETE FROM "t"; DELETE FROM "t" WHERE "a" = 1"#, r#"DELETE FROM "t" WHERE "a" = 2"#,
        "expected 2 queries, got 3", "not a pair"),
    row(Schema, r#"INSERT INTO "t" ("a") VALUES (1) ON CONFLICT DO UPDATE SET "b" = 1"#,
        r#"INSERT INTO "t" ("a") VALUES (1)"#, "without a conflict target",
        "42601 ON CONFLICT DO UPDATE requires inference specification or constraint name"),
    row(Schema, r#"SELECT "a" FROM "t" ORDER BY 1.5 LIMIT 1"#, r#"SELECT "a" FROM "t""#, "ORDER BY position 1.5",
        "42601 non-integer constant in ORDER BY"),
    row(Schema, r#"SELECT "a" FROM "t" ORDER BY 2 LIMIT 1"#, r#"SELECT "a" FROM "t""#,
        "ORDER BY position 2 out of range", "42P10 ORDER BY position 2 is not in select list"),
    row(Schema, r#"SELECT DISTINCT "a" FROM "t" ORDER BY "b" LIMIT 5"#, r#"SELECT "a" FROM "t""#,
        "of a SELECT DISTINCT", "42P10 for SELECT DISTINCT, ORDER BY expressions must appear in select list"),
    row(Schema, r#"SELECT "a" FROM "t" UNION SELECT "a" FROM "t" ORDER BY "b" LIMIT 5"#, r#"SELECT "a" FROM "t""#,
        "of a set operation", r#"42703 column "b" does not exist"#),
    row(Schema, r#"SELECT "a" FROM "t" WHERE ("a", "b") IN ((1, 2, 3))"#, r#"SELECT "a" FROM "t""#,
        "row IN arity mismatch", "42601 unequal number of entries in row expressions"),
    row(Schema, r#"SELECT "x" FROM (SELECT "a" FROM "t") AS "s"("x", "y")"#, r#"SELECT "a" FROM "t""#,
        "fewer columns than its alias names", "42P10 table has 1 columns available but 2 columns specified"),
    row(Schema, r#"SELECT sum() FILTER (WHERE "a" > 0) FROM "t""#, r#"SELECT count(*) FROM "t""#,
        "FILTER on argument-less SUM", "42883 function sum() does not exist"),
    row(Schema, r#"SELECT "a" FROM "t" GROUP BY 0"#, r#"SELECT "a" FROM "t""#, "GROUP BY position 0",
        "42P10 GROUP BY position 0 is not in select list"),
    row(Schema, r#"SELECT "a" FROM "t" GROUP BY 1.5"#, r#"SELECT "a" FROM "t""#, "GROUP BY position 1.5",
        "42601 non-integer constant in GROUP BY"),
    row(Schema, "SELECT *", "SELECT 1", "FROM-less SELECT *", "42601 SELECT * with no tables specified is not valid"),
    row(Schema, r#"SELECT "z".* FROM "t""#, r#"SELECT "a" FROM "t""#, "unknown qualifier z.*",
        r#"42P01 missing FROM-clause entry for table "z""#),
    row(Schema, r#"SELECT "a" AS "x", "b" AS "x" FROM "t" GROUP BY "x""#, r#"SELECT "a" FROM "t""#,
        "ambiguous GROUP BY key x", r#"42702 GROUP BY "x" is ambiguous"#),
    // A `RETURNING` item is a name the statement can see, as a select-list item is.
    row(Schema, r#"UPDATE "t" SET "a" = 1 RETURNING "zz""#, r#"UPDATE "t" SET "a" = 2 - 1 RETURNING "zz""#,
        "column zz does not exist", r#"42703 column "zz" does not exist"#),
    row(Schema, r#"DELETE FROM "t" RETURNING "zz""#, r#"DELETE FROM "t" WHERE true RETURNING "zz""#,
        "column zz does not exist", r#"42703 column "zz" does not exist"#),
    row(Schema, r#"INSERT INTO "t" ("a") VALUES (1) RETURNING "zz""#, r#"INSERT INTO "t" ("a") VALUES (2 - 1) RETURNING "zz""#,
        "column zz does not exist", r#"42703 column "zz" does not exist"#),
    row(Schema, r#"DELETE FROM "t" RETURNING "t"."zz""#, r#"DELETE FROM "t" WHERE true RETURNING "t"."zz""#,
        "column t.zz does not exist", r#"42703 column t.zz does not exist"#),
    row(Schema, r#"DELETE FROM "t" RETURNING "q".*"#, r#"DELETE FROM "t" WHERE true RETURNING "q".*"#,
        "missing FROM-clause entry for table q", r#"42P01 missing FROM-clause entry for table "q""#),
    row(Schema, r#"UPDATE "t" SET "a" = 1 RETURNING "old"."a""#, r#"UPDATE "t" SET "a" = 2 - 1 RETURNING "old"."a""#,
        "missing FROM-clause entry for table old", r#"42P01 missing FROM-clause entry for table "old" (Postgres 18 adds old and new)"#),
];

/// Input Postgres accepts.
const ACCEPTED: &[Row] = &[
    // Postgres syntax the frontend's parser does not read.
    row(Unsupported, "SELECT", "SELECT 1", "parser does not read", "prepares: a select list may be empty"),
    row(Unsupported, r#"SELECT "a" FROM ONLY "t""#, r#"SELECT "a" FROM "t""#, "ONLY in a table reference", "prepares"),
    row(Unsupported, r#"TABLE "t""#, r#"SELECT * FROM "t""#, "parser does not read", "prepares"),
    row(Unsupported, r#"DELETE FROM "t""#, r#"SELECT "a" FROM "t""#, "DELETE paired with non-DELETE", "prepares"),
    row(Unsupported, r#"MERGE INTO "t" USING "u" ON "t"."a" = "u"."a" WHEN MATCHED THEN DELETE"#,
        r#"MERGE INTO "t" USING "u" ON "t"."a" = "u"."a" WHEN MATCHED THEN UPDATE SET "b" = 1"#, "MERGE", "prepares"),
    row(Unsupported, r#"SELECT 1 FROM "t" AS "x"("b")"#, r#"SELECT 1 FROM "t""#, "two columns", "prepares"),
    row(Unsupported, r#"SELECT "a" FROM "t" WHERE EXISTS (SELECT "t".* FROM "u")"#, r#"SELECT "a" FROM "t""#,
        "over an enclosing query's binding", "prepares"),
    row(Unsupported, r#"SELECT "t" FROM "t""#, r#"SELECT "a" FROM "t""#, "whole-row reference t", "prepares"),
    row(Unsupported, r#"SELECT "a" AS "x", "a" AS "x" FROM "t" ORDER BY "x" LIMIT 5"#, r#"SELECT "a" FROM "t""#,
        "ambiguous ORDER BY key x", "prepares"),
    row(Unsupported, r#"SELECT "a" AS "x", "a" AS "x" FROM "t" GROUP BY "x""#, r#"SELECT "a" FROM "t""#,
        "names one expression twice", "prepares"),
    row(Unsupported, r#"SELECT "a", count(*) FROM "t" GROUP BY 1"#, r#"SELECT "a", count(*) FROM "t" GROUP BY "a""#,
        "GROUP BY position 1", "prepares"),
    row(Unsupported, r#"SELECT DISTINCT ON ("a") "a" FROM "t" ORDER BY "a", "b" LIMIT 1"#, r#"SELECT "a" FROM "t""#,
        "ORDER BY key is not an output column", "prepares"),
];

/// Tie-breaks: the input shown fails in Postgres, but the same refusal meets valid input too.
const TIE_BREAKS: &[Row] = &[
    // `SET c.x` assigns field `x` of a composite column `c`, which only the catalog can tell from
    // this table-qualified name.
    row(Unsupported, r#"UPDATE "t" SET "t"."b" = 1"#, r#"UPDATE "t" SET "b" = 1"#, "qualified SET target",
        r#"42703 column "t" of relation "t" does not exist"#),
    // A composite column is a row, so whether the right side is one is a question for the catalog.
    row(Unsupported, r#"SELECT "a" FROM "t" WHERE ("a", "b") = "a""#, r#"SELECT "a" FROM "t""#,
        "row comparison against a non-row operand", "42883 operator does not exist: record = integer"),
];

fn assert_refused(ddl: &str, r: &Row, sources: &[CatalogSource]) {
    let src = format!("{ddl}\n{};\n{};", r.a, r.b);
    for &source in sources {
        match (r.kind, lower_with(&src, source)) {
            (Parse, Err(FrontendError::Parse(m)))
            | (Schema, Err(FrontendError::Schema(m)))
            | (Unsupported, Err(FrontendError::Unsupported(m))) => {
                assert!(m.contains(r.reason), "{source:?}: refused for {m:?}, not {:?}\n{src}", r.reason)
            }
            (kind, other) => panic!(
                "{source:?}: expected a {kind:?} refusal mentioning {:?} (Postgres: {}), got {other:?}\n{src}",
                r.reason, r.postgres
            ),
        }
    }
}

#[test]
fn a_refusal_of_text_postgres_cannot_parse_is_parse() {
    for r in NOT_POSTGRES {
        assert_refused(T, r, &BOTH);
    }
}

#[test]
fn a_refusal_of_input_postgres_rejects_is_schema() {
    for r in REJECTED {
        assert_refused(T, r, &BOTH);
    }
}

#[test]
fn a_refusal_of_input_postgres_accepts_is_unsupported() {
    for r in ACCEPTED.iter().chain(TIE_BREAKS) {
        assert_refused(T, r, &BOTH);
    }
    // Only the inferring modes type `$N` and attribute a bare column. Each query types on its own in
    // Postgres; under one `$1` they would need two types.
    let typed = r#"create table "t" ("a" INTEGER, "b" VARCHAR, unique ("a"));"#;
    let conflict = row(Unsupported, r#"SELECT "a" FROM "t" WHERE "a" = $1 AND "b" = $2"#,
        r#"SELECT "a" FROM "t" WHERE "a" = $2 AND "b" = $1"#, "type conflict", "both sides prepare");
    assert_refused(typed, &conflict, &[CatalogSource::InferredSeeded]);
    // A name `JOIN ... USING` merged is one column to Postgres.
    let two = r#"create table "t" ("id" INTEGER, "x" INTEGER, unique ("id"));
create table "u" ("id" INTEGER, "x" INTEGER, unique ("id"));"#;
    let merged = row(Unsupported, r#"SELECT "x" FROM "t" JOIN "u" USING ("x")"#,
        r#"SELECT "t"."x" FROM "t" JOIN "u" USING ("x")"#, "ambiguous unqualified column x", "prepares");
    assert_refused(two, &merged, &[CatalogSource::InferredSeeded]);
}

#[test]
fn the_valid_neighbours_still_lower() {
    for (a, b) in [
        (r#"UPDATE "t" SET "a" = 1, "b" = 2"#, r#"UPDATE "t" SET "b" = 2, "a" = 1"#),
        (r#"SELECT "a" FROM "t" WHERE ("a", "b") IN ((1, 2))"#, r#"SELECT "a" FROM "t" WHERE "a" = 1 AND "b" = 2"#),
        (r#"SELECT "a" FROM "t" ORDER BY 1 LIMIT 1"#, r#"SELECT "a" FROM "t" ORDER BY "a" LIMIT 1"#),
        (r#"SELECT "x" FROM (SELECT "a" FROM "t") AS "s"("x")"#, r#"SELECT "a" FROM "t""#),
        // Postgres 16 and later accept a derived table with no alias.
        (r#"SELECT "a" FROM (SELECT "a" FROM "t")"#, r#"SELECT "a" FROM "t""#),
        // A shorter list renames the first columns and keeps the rest.
        (r#"SELECT "x", "b" FROM (SELECT "a", "b" FROM "t") AS "s"("x")"#, r#"SELECT "a", "b" FROM "t""#),
        (
            r#"SELECT "a" FROM "t" UNION SELECT "a" FROM "t" ORDER BY "a" LIMIT 5"#,
            r#"SELECT "a" FROM "t" UNION SELECT "a" FROM "t" ORDER BY 1 LIMIT 5"#,
        ),
        (r#"SELECT DISTINCT "a" FROM "t" ORDER BY "a" LIMIT 5"#, r#"SELECT DISTINCT "a" FROM "t" ORDER BY 1 LIMIT 5"#),
        // `RETURNING` names that do resolve: a system column, a wildcard, an alias, an expression, and
        // a subquery's own scope.
        (r#"UPDATE "t" SET "a" = 1 RETURNING "ctid", "t".*"#, r#"UPDATE "t" SET "a" = 2 - 1 RETURNING "ctid", "t".*"#),
        (r#"DELETE FROM "t" AS "x" RETURNING "x"."a" + 1 AS "n", *"#, r#"DELETE FROM "t" AS "x" WHERE true RETURNING "x"."a" + 1 AS "n", *"#),
        (r#"DELETE FROM "t" RETURNING (SELECT max("c") FROM "u")"#, r#"DELETE FROM "t" WHERE true RETURNING (SELECT max("c") FROM "u")"#),
        (r#"INSERT INTO "t" ("a") VALUES (1) RETURNING "a", "ctid""#, r#"INSERT INTO "t" ("a") VALUES (2 - 1) RETURNING "a", "ctid""#),
        (r#"UPDATE "t" SET "a" = 1 RETURNING make_interval(secs => "b")"#,
            r#"UPDATE "t" SET "a" = 2 - 1 RETURNING make_interval(secs => "b")"#),
    ] {
        let src = format!("{T}\n{a};\n{b};");
        for source in BOTH {
            lower_with(&src, source).unwrap_or_else(|e| panic!("{source:?}: {e}\n{src}"));
        }
    }
}
