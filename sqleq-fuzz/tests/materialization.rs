// Unless explicitly stated otherwise all files in this repository are licensed under the
// Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/).
// Copyright 2026-Present Datadog, Inc.

//! The generated instance is one Postgres would accept, and every table is the table Postgres would
//! resolve (issue #64).

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

/// DuckDB will not index a JSON operator, so such a key is enforced by checking each insert instead
/// of costing the pair its trials.
#[test]
fn a_unique_expression_duckdb_will_not_index_is_still_enforced() {
    let ddl = r#"create table "t" ("id" INTEGER NOT NULL, "j" JSONB, unique ("id"));
                 create unique index "t_j_a" on "t" (("j" ->> 'a'))"#;
    assert_eq!(
        label(
            r#"SELECT "j" ->> 'a' AS "k" FROM "t" WHERE "j" ->> 'a' IS NOT NULL"#,
            r#"SELECT DISTINCT "j" ->> 'a' AS "k" FROM "t" WHERE "j" ->> 'a' IS NOT NULL"#,
            ddl
        ),
        "NO-COUNTEREXAMPLE"
    );
    // NULL keys never collide, so they stay, and DISTINCT does remove those.
    assert_eq!(
        label(
            r#"SELECT "j" ->> 'a' AS "k" FROM "t""#,
            r#"SELECT DISTINCT "j" ->> 'a' AS "k" FROM "t""#,
            ddl
        ),
        "NOT-EQUIVALENT"
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

// -- The DDL applies in the order it is written (issue #143) -------------------------------------

/// The column names of table `t`, each with `!` when NOT NULL.
fn cols(ddl: &str, t: &str) -> Vec<String> {
    parse_schema(ddl)[t]
        .cols
        .iter()
        .map(|c| format!("{}{}", c.name, if c.notnull { "!" } else { "" }))
        .collect()
}

fn keys(ddl: &str, t: &str) -> Vec<Vec<String>> {
    parse_schema(ddl)[t].keys.clone()
}

#[test]
fn a_dropped_not_null_is_dropped_where_it_is_written() {
    let create = r#"create table "t" ("id" INTEGER NOT NULL, "a" INTEGER)"#;
    let drop = r#"alter table "t" alter column "id" drop not null"#;
    let set = r#"alter table "t" alter column "id" set not null"#;
    assert_eq!(cols(&format!("{create}; {drop}"), "t"), ["id", "a"]);
    assert_eq!(cols(&format!("{create}; {drop}; {set}"), "t"), ["id!", "a"]);
    assert_eq!(cols(&format!("{create}; {set}; {drop}"), "t"), ["id", "a"]);
    // Postgres refuses to drop a primary key's NOT NULL, and dropping the key leaves it.
    let pk = r#"create table "t" ("id" INTEGER PRIMARY KEY, "a" INTEGER)"#;
    assert_eq!(cols(&format!("{pk}; {drop}"), "t"), ["id!", "a"]);
    assert_eq!(
        cols(&format!(r#"{pk}; alter table "t" drop constraint "t_pkey""#), "t"),
        ["id!", "a"]
    );
    assert_eq!(
        label(
            r#"SELECT "a" FROM "t" WHERE "id" IS NOT NULL"#,
            r#"SELECT "a" FROM "t""#,
            &format!("{create}; {drop}")
        ),
        "NOT-EQUIVALENT"
    );
}

#[test]
fn a_renamed_column_keeps_its_not_null_and_its_keys() {
    // Three renames swap `a` and `b`: the NOT NULL, and the key, go with the column.
    let ddl = r#"create table "t" ("a" INTEGER NOT NULL UNIQUE, "b" INTEGER);
                 alter table "t" rename column "a" to "c";
                 alter table "t" rename column "b" to "a";
                 alter table "t" rename column "c" to "b""#;
    assert_eq!(cols(ddl, "t"), ["b!", "a"]);
    assert_eq!(keys(ddl, "t"), [["b"]]);
    // Read as declared, `b` drew NULLs and this equivalent pair was refuted.
    assert_eq!(
        label(
            r#"SELECT "b" FROM "t" WHERE "b" IS NOT NULL"#,
            r#"SELECT "b" FROM "t""#,
            ddl
        ),
        "NO-COUNTEREXAMPLE"
    );
    // A renamed table is found by its new name.
    let s = parse_schema(r#"create table "t" ("a" INTEGER); alter table "t" rename to "u""#);
    assert!(s.contains_key("u") && !s.contains_key("t"), "{:?}", s.keys());
    // A column the DDL never declared cannot be renamed into the table's picture of itself.
    let s = parse_schema(r#"create table "t" ("a" INTEGER); alter table "t" rename column "z" to "y""#);
    assert!(s["t"].unreadable.is_some());
}

#[test]
fn a_dropped_key_is_dropped_only_when_its_name_is_sure() {
    let t = r#"create table "t" ("id" INTEGER NOT NULL, "a" INTEGER)"#;
    for (ddl, kept) in [
        // Named by the DDL, or by Postgres (`t_pkey`, `t_a_key`, `t_id_idx`).
        (r#"alter table "t" add constraint "k" unique ("id"); alter table "t" drop constraint "k""#, 0),
        (r#"alter table "t" add primary key ("id"); alter table "t" drop constraint "t_pkey""#, 0),
        (r#"alter table "t" add unique ("a"); alter table "t" drop constraint "t_a_key""#, 0),
        (r#"create unique index on "t" ("id"); drop index "t_id_idx""#, 0),
        (r#"create unique index "i" on "t" ("id"); drop index "i""#, 0),
        (r#"create unique index "i" on "t" ("id"); alter index "i" rename to "j"; drop index "j""#, 0),
        (r#"create unique index "i" on "t" ("id"); alter index "i" rename to "j"; drop index "i""#, 1),
        // A column dropped takes its keys along.
        (r#"alter table "t" add unique ("a"); alter table "t" drop column "a""#, 0),
        // A CHECK named `t_a_key` first makes Postgres name the key `t_a_key1`.
        (
            r#"alter table "t" add constraint "t_a_key" check ("a" > 0); alter table "t" add unique ("a"); alter table "t" drop constraint "t_a_key""#,
            1,
        ),
        // `"I"` and `i` are two names.
        (r#"create unique index "I" on "t" ("id"); drop index "i""#, 1),
        // A name this cannot tell drops nothing.
        (r#"alter table "t" add unique ("a"); alter table "t" drop constraint "other""#, 1),
    ] {
        assert_eq!(keys(&format!("{t}; {ddl}"), "t").len(), kept, "{ddl}");
    }
    // An index name qualified by its schema names that schema's index alone.
    let ddl = r#"create table "s1"."t" ("a" INTEGER); create table "s2"."t" ("a" INTEGER);
                 create unique index "i" on "s1"."t" ("a"); create unique index "i" on "s2"."t" ("a")"#;
    let s = parse_schema(&format!(r#"{ddl}; drop index "s2"."i""#));
    assert_eq!((s["s1.t"].keys.len(), s["s2.t"].keys.len()), (1, 0));
    // Unqualified, it could be either, and both stay.
    let s = parse_schema(&format!(r#"{ddl}; drop index "i""#));
    assert_eq!((s["s1.t"].keys.len(), s["s2.t"].keys.len()), (1, 1));
}

#[test]
fn a_statement_before_its_table_is_read_as_before() {
    // A key or NOT NULL for a table declared later still lands on it; a drop does nothing.
    let ddl = r#"alter table "t" add primary key ("id");
                 alter table "t" alter column "a" drop not null;
                 create table "t" ("id" INTEGER, "a" INTEGER NOT NULL)"#;
    assert_eq!(cols(ddl, "t"), ["id!", "a!"]);
    assert_eq!(keys(ddl, "t"), [["id"]]);
    // A table dropped and declared again is the second one.
    let ddl = r#"create table "t" ("a" INTEGER); drop table "t"; create table "t" ("b" INTEGER NOT NULL)"#;
    assert_eq!(cols(ddl, "t"), ["b!"]);
}

#[test]
fn a_table_created_like_another_copies_it_as_it_stands() {
    let o = r#"create table "o" ("id" INTEGER PRIMARY KEY, "n" INTEGER NOT NULL, "x" INTEGER)"#;
    // The columns and their NOT NULLs always, the keys only with the indexes.
    let ddl = format!(r#"{o}; create table "t" (like "o")"#);
    assert_eq!(cols(&ddl, "t"), ["id!", "n!", "x"]);
    assert!(keys(&ddl, "t").is_empty());
    for like in [
        r#"like "o" including indexes"#,
        r#"like "o" including all"#,
        "like o including defaults including indexes",
    ] {
        assert_eq!(keys(&format!(r#"{o}; create table "t" ({like})"#), "t"), [["id"]], "{like}");
    }
    let ddl = format!(r#"{o}; create table "t" (like "o" including all excluding indexes)"#);
    assert!(keys(&ddl, "t").is_empty());
    // Where the item stands among the table's own columns.
    assert_eq!(
        cols(&format!(r#"{o}; create table "t" ("z" INTEGER, like "o", "w" INTEGER NOT NULL)"#), "t"),
        ["z", "id!", "n!", "x", "w!"]
    );
    // As the source stands at that point.
    let ddl = format!(r#"{o}; create table "t" (like "o"); alter table "o" add column "y" INTEGER"#);
    assert_eq!(cols(&ddl, "t"), ["id!", "n!", "x"]);
    // A source the DDL does not declare, or an option this does not know, withholds the table.
    for ddl in [
        r#"create table "t" (like "elsewhere")"#.to_string(),
        format!(r#"{o}; create table "t" (like "o" including everything)"#),
    ] {
        assert!(parse_schema(&ddl)["t"].unreadable.is_some(), "{ddl}");
    }
    assert_eq!(
        label(
            r#"SELECT DISTINCT "id" FROM "t""#,
            r#"SELECT "id" FROM "t""#,
            &format!(r#"{o}; create table "t" (like "o")"#)
        ),
        "NOT-EQUIVALENT"
    );
    assert_eq!(
        label(
            r#"SELECT DISTINCT "id" FROM "t""#,
            r#"SELECT "id" FROM "t""#,
            &format!(r#"{o}; create table "t" (like "o" including indexes)"#)
        ),
        "NO-COUNTEREXAMPLE"
    );
}

#[test]
fn inheritance_is_withheld_on_duckdb() {
    let ddl = r#"create table "p" ("id" INTEGER PRIMARY KEY);
                 create table "c" ("id" INTEGER NOT NULL, "x" INTEGER) inherits ("p")"#;
    // The child has its parent's columns first, its own NOT NULL merged in, and no key of the
    // parent.
    assert_eq!(cols(ddl, "c"), ["id!", "x"]);
    assert!(keys(ddl, "c").is_empty());
    // DuckDB has no inheritance: a scan of the parent would miss the child's rows.
    for t in ["p", "c"] {
        let v = label(
            &format!(r#"SELECT DISTINCT "id" FROM "{t}""#),
            &format!(r#"SELECT "id" FROM "{t}""#),
            ddl,
        );
        assert!(v.starts_with("NOT-COMPARABLE:") && v.contains("inheritance"), "{v}");
    }
}
