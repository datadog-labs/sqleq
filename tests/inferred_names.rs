// Unless explicitly stated otherwise all files in this repository are licensed under the
// Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/).
// Copyright 2026-Present Datadog, Inc.

//! Names under type inference, folded as Postgres folds them.
//!
//! Postgres folds an unquoted identifier to lower case (ASCII only, under a multibyte encoding) and
//! keeps a quoted one as written, so `"Orders"` and `orders` are two tables and `"createdAt"` is
//! not `createdat`. Lowering has resolved names that way since the declared catalog learned to; type
//! inference, which builds the catalog when there is no DDL and picks a column's table when there
//! is one, lower-cased every name instead. The pairs below are the ones that showed it: two
//! tables that inference merged into one, so a non-equivalent pair lowered to one query, and quoted
//! names that inference attributed to a column lowering never reads, so an equivalent pair was
//! refused. Controls keep the names that are one name lowering as one. They do not run a prover;
//! they pin the lowering.

use serde_json::Value;
use sqleq_frontend::{lower_with, CatalogSource, FrontendError};

fn lowered(src: &str, catalog: CatalogSource) -> Value {
    lower_with(src, catalog).unwrap_or_else(|e| panic!("expected Ok under {catalog:?}, got {e}"))
}

fn identical(src: &str, catalog: CatalogSource) -> bool {
    let v = lowered(src, catalog);
    v["queries"][0] == v["queries"][1]
}

/// The pair is refused with a message containing `needle`.
fn refused(src: &str, catalog: CatalogSource, needle: &str) {
    match lower_with(src, catalog) {
        Err(FrontendError::Unsupported(m) | FrontendError::Schema(m)) => {
            assert!(m.contains(needle), "refused for {m:?}, expected {needle:?}")
        }
        Err(e) => panic!("expected a refusal mentioning {needle:?}, got {e}"),
        Ok(_) => panic!("expected a refusal mentioning {needle:?} under {catalog:?}, but it lowered"),
    }
}

// --- two tables that differ only in case, without a DDL ---------------------------------------------

/// `"Orders"` and `orders` are two tables. Inference lower-cased both to `orders` and synthesized
/// one table, so the two sides lowered to one scan of it.
#[test]
fn a_quoted_table_and_its_folded_spelling_are_not_one_table() {
    refused(
        r#"SELECT "id" FROM "Orders"; SELECT "id" FROM "orders";"#,
        CatalogSource::Inferred,
        "two tables named orders up to case",
    );
}

/// The schema qualifier is the same failure: `strip_schema` keeps `"S".t` and `s.t` qualified, and
/// inference then lower-cased both to `s.t`.
#[test]
fn a_quoted_schema_and_its_folded_spelling_are_not_one_schema() {
    refused(
        r#"SELECT "a" FROM "S"."t"; SELECT "a" FROM "s"."t";"#,
        CatalogSource::Inferred,
        "two tables named s.t up to case",
    );
}

/// With both spellings in one `FROM`, the two sides were the two columns of a self-join of one
/// synthesized table, which are equal as bags.
#[test]
fn both_spellings_in_one_from_are_not_a_self_join() {
    refused(
        r#"SELECT "Orders"."id" FROM "Orders", "orders"; SELECT "orders"."id" FROM "Orders", "orders";"#,
        CatalogSource::Inferred,
        "two tables named orders up to case",
    );
}

/// The refusal fires wherever inference runs, the seeded catalog included: there inference attributes
/// columns by the folded table name too, and lowering finds both spellings in one declared table.
#[test]
fn the_seeded_catalog_refuses_both_spellings_too() {
    refused(
        "create table orders (id INTEGER);\nSELECT id FROM \"Orders\";\nSELECT id FROM orders;",
        CatalogSource::InferredSeeded,
        "two tables named orders up to case",
    );
}

// --- controls: what is one name stays one -----------------------------------------------------------

/// `Orders`, `ORDERS` and `"orders"` all fold to `orders`: one table, one query.
#[test]
fn unquoted_spellings_of_one_table_are_one_table() {
    for src in [
        r#"SELECT id FROM Orders; SELECT id FROM "orders";"#,
        r#"SELECT ID FROM ORDERS; SELECT "id" FROM orders;"#,
        r#"SELECT o.id FROM Orders O; SELECT "o".id FROM "orders" "o";"#,
    ] {
        assert!(identical(src, CatalogSource::Inferred), "{src} should be one query");
    }
}

/// A quoted mixed-case table read consistently on both sides is one table, as it always was.
#[test]
fn a_quoted_mixed_case_table_read_on_both_sides_lowers() {
    let src = r#"SELECT "id" FROM "Orders" WHERE "id" > 1; SELECT "id" FROM "Orders" WHERE 1 < "id";"#;
    lowered(src, CatalogSource::Inferred);
    let src = r#"SELECT "a" FROM "S"."t"; SELECT "S"."t"."a" FROM "S"."t";"#;
    assert!(identical(src, CatalogSource::Inferred));
}

// --- quoted columns ---------------------------------------------------------------------------------

/// Inference synthesized `createdat` for `"createdAt"`, and lowering, which keeps the quoted case,
/// could not find it: `unresolved column createdAt`.
#[test]
fn a_quoted_mixed_case_column_lowers_without_a_ddl() {
    let src = r#"SELECT "id" FROM "t" WHERE "createdAt" > 1; SELECT "id" FROM "t" WHERE 1 < "createdAt";"#;
    let v = lowered(src, CatalogSource::Inferred);
    // One synthesized table, `t (createdAt, id)`, with the comparison typing the column.
    assert_eq!(v["schemas"].as_array().map(Vec::len), Some(1), "{}", v["schemas"]);
    assert_eq!(v["schemas"][0]["types"].as_array().map(Vec::len), Some(2), "{}", v["schemas"]);
}

/// `"A"` and `a` are two columns, so the two sides read two columns of `t`.
#[test]
fn a_quoted_column_and_its_folded_spelling_are_two_columns() {
    let src = r#"SELECT "createdAt" FROM t; SELECT createdat FROM t;"#;
    let v = lowered(src, CatalogSource::Inferred);
    assert_ne!(v["queries"][0], v["queries"][1], "\"createdAt\" and createdat read one column");
    assert_eq!(v["schemas"][0]["types"].as_array().map(Vec::len), Some(2), "{}", v["schemas"]);
}

/// `"a"`, `a` and `A` are one column.
#[test]
fn quoted_and_unquoted_spellings_of_one_column_are_one_column() {
    let src = r#"SELECT "a" FROM t; SELECT A FROM t;"#;
    assert!(identical(src, CatalogSource::Inferred));
    let v = lowered(src, CatalogSource::Inferred);
    assert_eq!(v["schemas"][0]["types"].as_array().map(Vec::len), Some(1), "{}", v["schemas"]);
}

/// An unquoted non-ASCII letter is not folded: under a multibyte encoding Postgres lower-cases only
/// ASCII. Inference lower-cased `É` to `é`, and lowering looked up `É`.
#[test]
fn an_unquoted_non_ascii_column_keeps_its_case() {
    let src = "SELECT É FROM t WHERE É > 1; SELECT É FROM t WHERE 1 < É;";
    lowered(src, CatalogSource::Inferred);
    let src = "SELECT É FROM t; SELECT é FROM t;";
    let v = lowered(src, CatalogSource::Inferred);
    assert_ne!(v["queries"][0], v["queries"][1], "É and é read one column");
}

// --- the seeded catalog picks the column lowering reads -----------------------------------------------

const M_T: &str = "create table \"m\" (\"A\" INTEGER);\ncreate table \"t\" (\"a\" INTEGER);\n";

// Each pair below reads a column of both tables. The seeded catalog still refuses a declared table
// that no column is attributed to (`table without referenced columns`), which is a separate defect.

/// `"A"` over `m ("A")` and `t (a)` names only `m`'s column. Matched up to case, inference found it
/// in both tables and refused the column as ambiguous.
#[test]
fn a_quoted_column_is_declared_by_the_table_with_that_exact_name() {
    let src = format!(
        r#"{M_T}SELECT "A", "t"."a" FROM "m", "t"; SELECT "m"."A", "t"."a" FROM "m", "t";"#
    );
    assert!(identical(&src, CatalogSource::InferredSeeded));
    // The declared catalog has always resolved it.
    assert!(identical(&src, CatalogSource::Declared));
}

/// An unquoted `A` folds to `a`, which is `t`'s column, as Postgres reads it.
#[test]
fn an_unquoted_column_is_declared_by_the_table_with_its_folded_name() {
    let src = format!(
        r#"{M_T}SELECT A, "m"."A" FROM "m", "t"; SELECT "t"."a", "m"."A" FROM "m", "t";"#
    );
    assert!(identical(&src, CatalogSource::InferredSeeded));
    assert!(identical(&src, CatalogSource::Declared));
}

/// And the two are two columns: `"A"` is `m`'s, `a` is `t`'s.
#[test]
fn the_seeded_catalog_keeps_a_quoted_column_apart_from_its_folded_spelling() {
    let src = format!(r#"{M_T}SELECT "A" FROM "m", "t"; SELECT a FROM "m", "t";"#);
    assert!(!identical(&src, CatalogSource::InferredSeeded));
    assert!(!identical(&src, CatalogSource::Declared));
}

/// A control: a `$N` compared with a quoted mixed-case column still takes the declared type through
/// the seed, now matched by the exact folded name rather than up to case.
#[test]
fn a_quoted_column_seeds_its_declared_type() {
    let src = "create table t (id INTEGER, \"createdAt\" VARCHAR);\n\
               SELECT id FROM t WHERE \"createdAt\" = $1;\n\
               SELECT id FROM t WHERE $1 = \"createdAt\";";
    let v = lowered(src, CatalogSource::InferredSeeded);
    let text = v.to_string();
    // The parameter is declared VARCHAR, as the column is, not left an opaque VARBINARY.
    assert!(!text.contains("VARBINARY"), "{text}");
}
