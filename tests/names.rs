// Unless explicitly stated otherwise all files in this repository are licensed under the
// Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/).
// Copyright 2026-Present Datadog, Inc.

//! Names that two different relations or columns share after a rewrite, and the keyword `DEFAULT`
//! read as a name.
//!
//! Postgres folds an unquoted identifier to lower case and keeps a quoted one as written, and a
//! schema qualifier is part of a table's name. Each test below is a pair that is **not** equivalent
//! in Postgres and that used to lower to one query, or to settle as `reflexive`, because a name was
//! compared without its qualifier or without its quoting. Controls beside them keep the shapes that
//! are one name lowering as before. They do not run a prover; they pin the lowering.

use serde_json::Value;
use sqleq_frontend::{lower_with, reflexive, CatalogSource, FrontendError};

const T: &str = r#"create table "t" ("a" INTEGER, "b" INTEGER);"#;

fn lowered(src: &str, catalog: CatalogSource) -> Value {
    lower_with(src, catalog).unwrap_or_else(|e| panic!("expected Ok, got {e}"))
}

fn identical(src: &str, catalog: CatalogSource) -> bool {
    let v = lowered(src, catalog);
    v["queries"][0] == v["queries"][1]
}

fn refused(src: &str, needle: &str) {
    match lower_with(src, CatalogSource::Declared) {
        Err(FrontendError::Unsupported(m) | FrontendError::Schema(m)) => {
            assert!(m.contains(needle), "refused for {m:?}, expected {needle:?}")
        }
        Err(e) => panic!("expected a refusal mentioning {needle:?}, got {e}"),
        Ok(_) => panic!("expected a refusal mentioning {needle:?}, but it lowered"),
    }
}

/// Whether a lowered query keeps its row slice (a `sort` node), which is what
/// `strip_identical_pagination` removes from both sides.
fn has_sort(v: &Value) -> bool {
    match v {
        Value::Object(m) => m.contains_key("sort") || m.values().any(has_sort),
        Value::Array(a) => a.iter().any(has_sort),
        _ => false,
    }
}

// --- `strip_schema` decides for the pair, not per query -------------------------------------------

/// `s1.t` and `s2.t` are two tables. Each query qualifies its one table consistently, so a guard that
/// looks at one query at a time strips both to `t`, and the pair becomes one query.
#[test]
fn two_qualifiers_of_one_bare_name_across_the_pair_are_two_tables() {
    let pair = "create table s1.t (a INTEGER);\ncreate table s2.t (a INTEGER);\n\
                SELECT a FROM s1.t;\nSELECT a FROM s2.t;";
    assert!(!reflexive(pair), "s1.t and s2.t settled as one query");
    for catalog in [CatalogSource::Declared, CatalogSource::InferredSeeded, CatalogSource::Inferred] {
        assert!(!identical(pair, catalog), "s1.t and s2.t lowered to one scan under {catalog:?}");
    }
    // A DDL that declares the bare name: neither side is the declared table any more.
    refused("create table t (a INTEGER);\nSELECT a FROM s1.t;\nSELECT a FROM s2.t;", "unknown table");
    // Nor a qualifier on one side only: `t` and `s2.t` are not known to be one table.
    refused("create table t (a INTEGER);\nSELECT a FROM t;\nSELECT a FROM s2.t;", "unknown table");
}

/// A quoted qualifier keeps its case, so `"S1".t` and `s1.t` are two schemas' tables.
#[test]
fn qualifiers_are_compared_under_postgres_folding() {
    let pair = "create table t (a INTEGER);\nSELECT a FROM \"S1\".t;\nSELECT a FROM s1.t;";
    assert!(!reflexive(pair), "\"S1\".t and s1.t settled as one query");
    refused(pair, "unknown table");
}

/// Control: one qualifier, however it is spelled, still strips on both sides, which is what the
/// rewrite is for.
#[test]
fn one_qualifier_on_both_sides_still_strips() {
    // Lowered at all: the declared table is the bare `t`.
    lowered("create table t (a INTEGER);\nSELECT a FROM s1.t;\nSELECT a FROM \"s1\".T WHERE a > 0;", CatalogSource::Declared);
    assert!(identical("create table t (a INTEGER);\nSELECT a FROM s1.t;\nSELECT a FROM S1.t;", CatalogSource::Declared));
}

// --- A reference resolves against the declared catalog ---------------------------------------------

/// The declared tables each query of a lowered pair scans, by name.
fn scanned(src: &str) -> Vec<Vec<String>> {
    fn scans(v: &Value, out: &mut Vec<usize>) {
        match v {
            Value::Object(m) => {
                if let Some(i) = m.get("scan").and_then(Value::as_u64) {
                    out.push(i as usize);
                }
                m.values().for_each(|x| scans(x, out));
            }
            Value::Array(a) => a.iter().for_each(|x| scans(x, out)),
            _ => {}
        }
    }
    let v = lowered(src, CatalogSource::Declared);
    let names: Vec<String> =
        v["schemas"].as_array().unwrap().iter().map(|t| t["name"].as_str().unwrap().to_string()).collect();
    v["queries"]
        .as_array()
        .unwrap()
        .iter()
        .map(|q| {
            let mut out = Vec::new();
            scans(q, &mut out);
            out.into_iter().map(|i| names[i].clone()).collect()
        })
        .collect()
}

/// With both `t` and `s.t` declared, `FROM s.t` is `s.t`. Stripped to `t`, both queries read the
/// keyed `t`, and the `DISTINCT` over `s.t`'s duplicates was provably removable.
#[test]
fn a_qualified_reference_is_the_table_declared_under_that_name() {
    let pair = "create table t (a INTEGER PRIMARY KEY);\ncreate table s.t (a INTEGER);\n\
                SELECT DISTINCT a FROM s.t;\nSELECT a FROM s.t;";
    assert_eq!(scanned(pair), [["s.t"], ["s.t"]]);
    // A table declared only under a schema is found under that name too.
    let pair = "create table s.t (id INTEGER PRIMARY KEY, a INTEGER);\n\
                SELECT id FROM s.t WHERE a > 1;\nSELECT id FROM s.t WHERE 1 < a;";
    assert_eq!(scanned(pair), [["s.t"], ["s.t"]]);
}

/// A bare reference is the table declared bare, else `public`'s, else the one table of that name.
#[test]
fn a_bare_reference_resolves_to_the_one_declared_table_of_its_name() {
    let q = "SELECT a FROM t;\nSELECT a FROM t WHERE a > 0;";
    assert_eq!(scanned(&format!("create table s.t (a INTEGER);\n{q}")), [["s.t"], ["s.t"]]);
    assert_eq!(scanned(&format!("create table public.t (a INTEGER);\ncreate table s.t (a INTEGER);\n{q}")), [["public.t"], ["public.t"]]);
    assert_eq!(scanned(&format!("create table t (a INTEGER);\ncreate table public.t (a INTEGER);\n{q}")), [["t"], ["t"]]);
    // Two schemas and no bare or `public` table of that name: not known to be either.
    refused(&format!("create table a.t (a INTEGER);\ncreate table b.t (a INTEGER);\n{q}"), "unknown table t");
    // A bare name on one side and a qualified one on the other are still not known to be one table.
    refused("create table s.t (a INTEGER);\nSELECT a FROM t;\nSELECT a FROM s.t;", "unknown table t");
}

// --- `DEFAULT` is a keyword, not a column ---------------------------------------------------------

/// `SET a = DEFAULT` stores the column's default. sqlparser gives the keyword as an unquoted
/// identifier, which used to resolve to the column `"default"`.
#[test]
fn set_to_default_is_refused_not_read_as_a_column() {
    let ddl = r#"create table "t" ("id" INTEGER, "a" INTEGER DEFAULT 0, "default" INTEGER);"#;
    refused(&format!("{ddl}\nUPDATE \"t\" SET \"a\" = DEFAULT;\nUPDATE \"t\" SET \"a\" = \"default\";"), "DEFAULT");
    refused(&format!("{ddl}\nUPDATE t SET a = DEFAULT WHERE id = 1;\nUPDATE t SET a = 0 WHERE id = 1;"), "DEFAULT");
    // In a `VALUES` row of an `INSERT`, and in any case.
    refused(&format!("{ddl}\nINSERT INTO t (id, a) VALUES (1, default);\nINSERT INTO t (id, a) VALUES (1, 0);"), "DEFAULT");
    // Control: the quoted name and the qualified one are the column, as in Postgres.
    assert!(identical(
        &format!("{ddl}\nUPDATE t SET a = \"default\";\nUPDATE t SET a = t.\"default\";"),
        CatalogSource::Declared
    ));
}

// --- `strip_identical_pagination` reads a key as a name, under folding ----------------------------

/// The unquoted key `A` is `a`. No output column has that name (the alias is `"A"`), so in A it is
/// the input column `t.a`, which the projection drops, and the pagination has to stay.
#[test]
fn an_unquoted_key_does_not_match_a_quoted_alias_of_another_case() {
    let v = lowered(
        &format!(
            "{T}\nSELECT \"b\" AS \"A\" FROM \"t\" ORDER BY A LIMIT 1;\n\
             SELECT \"b\" AS \"A\" FROM (SELECT \"b\", \"b\" AS \"a\" FROM \"t\") AS \"v\" ORDER BY A LIMIT 1;"
        ),
        CatalogSource::Declared,
    );
    assert!(has_sort(&v["queries"][0]) && has_sort(&v["queries"][1]), "the pagination was stripped");
}

/// Only a bare-name key is looked up among the output names: `t.a` is the input column, whatever an
/// alias `"t.a"` spells.
#[test]
fn a_qualified_key_does_not_match_an_alias_spelled_like_it() {
    let v = lowered(
        &format!(
            "{T}\nSELECT \"b\" AS \"t.a\" FROM \"t\" ORDER BY t.a LIMIT 1;\n\
             SELECT \"b\" AS \"t.a\" FROM (SELECT \"b\", \"b\" AS \"a\" FROM \"t\") AS \"t\" ORDER BY t.a LIMIT 1;"
        ),
        CatalogSource::Declared,
    );
    assert!(has_sort(&v["queries"][0]) && has_sort(&v["queries"][1]), "the pagination was stripped");
}

/// Control: a key that names an alias under Postgres's folding still determines the page.
#[test]
fn a_key_that_names_the_alias_still_strips_the_pagination() {
    for (alias, key) in [("\"A\"", "\"A\""), ("A", "a"), ("a", "\"a\""), ("x", "X")] {
        let v = lowered(
            &format!(
                "{T}\nSELECT \"b\" AS {alias} FROM \"t\" ORDER BY {key} LIMIT 1;\n\
                 SELECT \"b\" AS {alias} FROM \"t\" WHERE \"a\" > 0 ORDER BY {key} LIMIT 1;"
            ),
            CatalogSource::Declared,
        );
        assert!(!has_sort(&v["queries"][0]), "alias {alias}, key {key}: the pagination was kept");
    }
}

// --- `strip_identical_pagination` needs the same output column on both sides ----------------------

const TID: &str = r#"create table "t" ("id" INTEGER, "a" INTEGER, "b" INTEGER, unique ("id"));"#;

/// A's key `a` is its second output column (`t.b`), B's is its first (`t.a`). Each is determined by
/// its own projection, and the two inner queries are the same bag, but the pages are not.
#[test]
fn one_key_naming_two_output_positions_keeps_the_pagination() {
    for tail in ["LIMIT 1", "LIMIT 1 OFFSET 1", "OFFSET 1"] {
        let v = lowered(
            &format!("{TID}\nSELECT a AS b, b AS a FROM t ORDER BY a {tail};\nSELECT a, b FROM t ORDER BY a {tail};"),
            CatalogSource::Declared,
        );
        assert!(has_sort(&v["queries"][0]) && has_sort(&v["queries"][1]), "{tail}: the pagination was stripped");
        assert_ne!(v["queries"][0], v["queries"][1], "{tail}: the two sides lowered alike");
    }
}

/// `CAST(a AS TEXT)` is named `a` by Postgres, so in A the key `a` is that column, sorted as text, and
/// in B, where the cast has an alias, it is the input column, sorted as a number. Only the strip is
/// checked here: lowering still reads A's key as the input column, which is a defect of its own.
#[test]
fn a_key_an_unnamed_expression_could_answer_to_keeps_the_pagination() {
    let v = lowered(
        &format!(
            "{TID}\nSELECT CAST(a AS TEXT), a AS z FROM t ORDER BY a LIMIT 1;\n\
             SELECT CAST(a AS TEXT) AS w, a AS z FROM t ORDER BY a LIMIT 1;"
        ),
        CatalogSource::Declared,
    );
    assert!(has_sort(&v["queries"][0]), "the pagination was stripped");
}

/// Control: the same key at the same position on both sides still strips, whatever it is called.
#[test]
fn one_key_at_one_output_position_still_strips_the_pagination() {
    for (a, b, key) in [
        ("SELECT a AS b, b AS a FROM t", "SELECT a AS b, b AS a FROM t WHERE id > 0", "a"),
        ("SELECT b, a FROM t", "SELECT b AS a, a AS b FROM t WHERE id > 0", "1"),
        ("SELECT a + 1, b FROM t", "SELECT a + 1 AS c, b FROM t WHERE id > 0", "a + 1"),
    ] {
        let v = lowered(&format!("{TID}\n{a} ORDER BY {key} LIMIT 1;\n{b} ORDER BY {key} LIMIT 1;"), CatalogSource::Declared);
        assert!(!has_sort(&v["queries"][0]) && !has_sort(&v["queries"][1]), "{a} | {b}: the pagination was kept");
    }
}

// --- `inline_ctes` matches a use to its binding under folding -------------------------------------

/// `"T"` is not `t`: the binding is unused, and `t` is still the base table.
#[test]
fn a_quoted_binding_does_not_capture_a_table_of_another_case() {
    assert!(identical(
        &format!("{T}\nWITH \"T\" AS (SELECT \"a\" + 1 AS \"a\" FROM \"t\") SELECT \"a\" FROM t;\nSELECT \"a\" FROM t;"),
        CatalogSource::Declared
    ));
    // Through the DML reduction, past its quote-aware shadowing check: the `DELETE` empties `t`.
    assert!(identical(
        &format!("{T}\nWITH \"T\" AS (SELECT * FROM t WHERE a = 1) DELETE FROM t;\nDELETE FROM t;"),
        CatalogSource::Declared
    ));
}

/// Control: a use that is the binding's name under folding is still inlined.
#[test]
fn a_use_named_like_its_binding_under_folding_is_inlined() {
    for (binding, using) in [("\"T\"", "\"T\""), ("c", "C"), ("c", "\"c\""), ("C", "c")] {
        assert!(
            identical(
                &format!(
                    "{T}\nWITH {binding} AS (SELECT \"a\" + 1 AS \"a\" FROM \"t\") SELECT \"a\" FROM {using};\n\
                     SELECT \"a\" FROM (SELECT \"a\" + 1 AS \"a\" FROM \"t\") AS v;"
                ),
                CatalogSource::Declared
            ),
            "WITH {binding} ... FROM {using}"
        );
    }
}

// --- A derived table's column names must stay apart once lower-cased ------------------------------

/// `"b"` and `"B"` are two columns of `s`. Both are stored as `b`, and `s."B"` used to read the first.
#[test]
fn a_derived_table_with_two_columns_named_alike_up_to_case_is_refused() {
    refused(
        &format!(
            "{T}\nSELECT \"s\".\"B\" FROM (SELECT \"a\" AS \"b\", \"b\" AS \"B\" FROM \"t\") AS \"s\";\n\
             SELECT \"s\".\"b\" FROM (SELECT \"a\" AS \"b\", \"b\" AS \"B\" FROM \"t\") AS \"s\";"
        ),
        "two columns named b",
    );
    // The same through the alias's column list.
    refused(
        &format!(
            "{T}\nSELECT \"s\".\"B\" FROM (SELECT \"a\", \"b\" FROM \"t\") AS \"s\"(\"b\", \"B\");\n\
             SELECT \"s\".\"b\" FROM (SELECT \"a\", \"b\" FROM \"t\") AS \"s\"(\"b\", \"B\");"
        ),
        "two columns named b",
    );
    // And through `*` over a join, which reads the two names out of the catalog.
    refused(
        "create table t (\"b\" INTEGER);\ncreate table u (\"B\" INTEGER);\n\
         SELECT s.\"B\" FROM (SELECT * FROM t, u) AS s;\nSELECT s.b FROM (SELECT * FROM t, u) AS s;",
        "two columns named b",
    );
}

/// Control: distinct names, quoted or not, still lower.
#[test]
fn a_derived_table_with_distinct_column_names_still_lowers() {
    assert!(!identical(
        &format!(
            "{T}\nSELECT \"s\".\"B\" FROM (SELECT \"a\" AS \"c\", \"b\" AS \"B\" FROM \"t\") AS \"s\";\n\
             SELECT \"s\".\"c\" FROM (SELECT \"a\" AS \"c\", \"b\" AS \"B\" FROM \"t\") AS \"s\";"
        ),
        CatalogSource::Declared
    ));
}
