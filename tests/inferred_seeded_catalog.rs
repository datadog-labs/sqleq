// Unless explicitly stated otherwise all files in this repository are licensed under the
// Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/).
// Copyright 2026-Present Datadog, Inc.

//! The inferred-seeded catalog lowers what the declared catalog lowers (issue #111).
//!
//! The seeded mode infers parameter and function types with the DDL as evidence, then lowers
//! against the declared catalog. Inference used to synthesize a catalog of its own as well, from
//! the columns the queries name, and to refuse the pair when it could not: `no base tables`, or
//! `table without referenced columns: t`, for a table read only through `*`, `count(*)`, a constant
//! or a `USING` list. The seeded mode then discarded that catalog, so it refused pairs the declared
//! catalog lowers, a `$N` pair no catalog then lowered, and a refusal of the declared catalog's own
//! was hidden behind one about a catalog nobody read.
//!
//! Under `--infer` the synthesized catalog is the one lowering reads, and a table with no column to
//! synthesize is still refused; the tests below pin what the refusal now says. They do not run a
//! prover; they pin the lowering.

use serde_json::Value;
use sqleq_frontend::{lower_with, CatalogSource, FrontendError};

/// Issue #111's first pair: `t` is read only through `count(*)`, and `$N` keeps the declared catalog
/// from lowering it at all, so no catalog lowered it.
const COUNT_PARAM: &str = r#"create table "t" ("a" INTEGER, "b" INTEGER);
create table "u" ("a" INTEGER, "b" INTEGER);
SELECT count(*) FROM "t", "u" WHERE "u"."a" = $1;
SELECT count(*) FROM "u", "t" WHERE "u"."a" = $1;"#;

/// The second: no column of any table is named.
const COUNT_STAR: &str = r#"create table "t" ("a" INTEGER, "b" INTEGER);
SELECT count(*) FROM "t";
SELECT count(*) FROM (SELECT * FROM "t") AS "x";"#;

/// The third: a bare `t` and a qualified `s.t`, both declared, are two tables. Not equivalent: under
/// the default `search_path`, with `t = {(1, 1)}` and `s.t = {}`, A returns `(1, 1)` and B no rows.
const BARE_VS_QUALIFIED: &str = r#"create table "t" ("a" INTEGER, "b" INTEGER);
create table "s"."t" ("a" INTEGER, "b" INTEGER);
SELECT * FROM "t" ORDER BY "a";
SELECT "a", "b" FROM "s"."t" ORDER BY "a", "b";"#;

/// The same queries with only the bare `t` declared, so `s.t` is a table the DDL lacks.
const QUALIFIED_UNDECLARED: &str = r#"create table "t" ("a" INTEGER, "b" INTEGER);
SELECT * FROM "t" ORDER BY "a";
SELECT "a", "b" FROM "s"."t" ORDER BY "a", "b";"#;

fn lowered(src: &str, catalog: CatalogSource) -> Value {
    lower_with(src, catalog).unwrap_or_else(|e| panic!("expected Ok under {catalog:?}, got {e}"))
}

fn refusal(src: &str, catalog: CatalogSource) -> FrontendError {
    match lower_with(src, catalog) {
        Err(e) => e,
        Ok(_) => panic!("expected a refusal under {catalog:?}, but it lowered"),
    }
}

/// The pair lowers under the seeded catalog to exactly the IR the declared catalog gives it.
fn seeded_is_declared(src: &str) -> Value {
    let declared = lowered(src, CatalogSource::Declared);
    let seeded = lowered(src, CatalogSource::InferredSeeded);
    assert_eq!(seeded.to_string(), declared.to_string(), "the seeded IR is not the declared IR");
    seeded
}

/// A refusal as its kind and message, for comparing two modes' answers.
fn kind_and_message(e: &FrontendError) -> (&'static str, String) {
    match e {
        FrontendError::Parse(m) => ("parse", m.clone()),
        FrontendError::Unsupported(m) => ("unsupported", m.clone()),
        FrontendError::Schema(m) => ("schema", m.clone()),
        FrontendError::ParameterMisaligned(m) => ("parameter-misaligned", m.clone()),
    }
}

// --- the seeded catalog lowers what the declared catalog lowers ---------------------------------------

/// A `$N` pair whose `t` is read only through `count(*)` lowers, with the parameter's type inferred.
#[test]
fn a_parameterized_pair_with_a_table_read_only_through_count_star_lowers() {
    lowered(COUNT_PARAM, CatalogSource::InferredSeeded);
    // The declared catalog refuses a bare placeholder, so the seeded mode is the one that lowers it.
    assert!(matches!(refusal(COUNT_PARAM, CatalogSource::Declared), FrontendError::Unsupported(_)));
}

/// With no column named anywhere the two sides are one query, as under the declared catalog.
#[test]
fn a_pair_that_names_no_column_lowers_to_one_query() {
    let v = seeded_is_declared(COUNT_STAR);
    assert_eq!(v["queries"][0], v["queries"][1]);
}

/// A bare `t` and a qualified `s.t` are two scans of two tables.
#[test]
fn a_bare_and_a_qualified_name_are_two_scans() {
    let v = seeded_is_declared(BARE_VS_QUALIFIED);
    assert_eq!(v["schemas"].as_array().map(Vec::len), Some(2), "{}", v["schemas"]);
    assert_ne!(v["queries"][0], v["queries"][1], "t and s.t lowered to one query");
}

/// Pinned pairs whose tables are read only through a constant or a `USING` list lower to the IR the
/// declared catalog gives them: `SELECT TRUE FROM t`, `SELECT 1 FROM t` with a subquery over `s`,
/// and a `v` that only `USING (a)` reads.
#[test]
fn pinned_pairs_lower_as_under_the_declared_catalog() {
    for src in [
        include_str!("pairs/types/true_vs_one.sql"),
        include_str!("pairs/subqueries/empty_table_sum.sql"),
        include_str!("pairs/joins/using_after_left_join.sql"),
    ] {
        seeded_is_declared(src);
    }
}

// --- and refuses what it refuses, for the same reason ------------------------------------------------

/// A set-returning function in `FROM` is refused for what it is, not for having no column named.
#[test]
fn a_set_returning_function_in_from_is_refused_for_the_declared_catalogs_reason() {
    let src = include_str!("pairs/refusals/srf_in_from.sql");
    let declared = kind_and_message(&refusal(src, CatalogSource::Declared));
    let seeded = kind_and_message(&refusal(src, CatalogSource::InferredSeeded));
    assert_eq!(seeded, declared);
    assert_eq!(seeded, ("unsupported", "table factor with table-valued function arguments".into()));
}

/// A table the DDL lacks is named, as under the declared catalog.
#[test]
fn a_table_the_ddl_lacks_is_refused_as_unknown() {
    let declared = kind_and_message(&refusal(QUALIFIED_UNDECLARED, CatalogSource::Declared));
    let seeded = kind_and_message(&refusal(QUALIFIED_UNDECLARED, CatalogSource::InferredSeeded));
    assert_eq!(seeded, declared);
    assert_eq!(seeded.0, "schema");
    assert!(seeded.1.starts_with("unknown table s.t"), "{}", seeded.1);
}

/// Control: two spellings of one name up to case are still refused under the seeded catalog, though
/// only `orders` is declared and the read of `"Orders"` is therefore the one Postgres would reject.
#[test]
fn two_spellings_up_to_case_are_still_refused() {
    let src = "create table orders (id INTEGER);\nSELECT count(*) FROM \"Orders\";\nSELECT count(*) FROM orders;";
    match refusal(src, CatalogSource::InferredSeeded) {
        FrontendError::Unsupported(m) => assert!(m.contains("two tables named orders up to case"), "{m}"),
        e => panic!("expected the up-to-case refusal, got {e}"),
    }
}

// --- `--infer`: the refusal stays, and says why -------------------------------------------------------

/// A pair that names a table but no column of it: a schema refusal that names the table and the
/// cause, not `no base tables`.
#[test]
fn under_infer_a_table_with_no_named_column_is_named() {
    match refusal(COUNT_STAR, CatalogSource::Inferred) {
        FrontendError::Schema(m) => {
            assert!(!m.contains("no base tables"), "{m}");
            assert!(m.contains(": t ("), "{m}");
            assert!(m.contains("read only through *, count(*), a constant or a USING list"), "{m}");
        }
        e => panic!("expected a schema refusal, got {e}"),
    }
}

/// `no base tables` is kept for a pair that names no table at all.
#[test]
fn under_infer_no_base_tables_means_no_table() {
    match refusal("SELECT 1 + 1;\nSELECT 2;", CatalogSource::Inferred) {
        FrontendError::Schema(m) => assert_eq!(m, "no base tables"),
        e => panic!("expected a schema refusal, got {e}"),
    }
}

/// A table without a column, beside one with: the same kind and words as before, and the cause.
#[test]
fn under_infer_a_table_without_referenced_columns_says_why() {
    match refusal(COUNT_PARAM, CatalogSource::Inferred) {
        FrontendError::Unsupported(m) => {
            assert!(m.contains("table without referenced columns: t ("), "{m}");
            assert!(m.contains("read only through *, count(*), a constant or a USING list"), "{m}");
            assert!(m.contains("so its columns cannot be inferred"), "{m}");
            assert!(!m.contains("two tables"), "{m}");
        }
        e => panic!("expected an unsupported refusal, got {e}"),
    }
}

/// And a bare `t` beside a qualified `s.t` is said to be two tables.
#[test]
fn under_infer_a_bare_and_a_qualified_name_are_said_to_be_two_tables() {
    match refusal(QUALIFIED_UNDECLARED, CatalogSource::Inferred) {
        FrontendError::Unsupported(m) => {
            assert!(m.contains("table without referenced columns: t ("), "{m}");
            assert!(m.contains("s.t and t are two tables"), "{m}");
        }
        e => panic!("expected an unsupported refusal, got {e}"),
    }
}

// --- the declared catalog: an unknown table names the tables with its last name -------------------

/// `s.t` against a DDL that declares `t`, and the reverse, which `strip_schema` also produces when
/// both queries name `s.t`: the message says what the catalog has, and nothing about why.
#[test]
fn an_unknown_table_names_the_declared_tables_with_its_last_name() {
    match refusal(QUALIFIED_UNDECLARED, CatalogSource::Declared) {
        FrontendError::Schema(m) => assert_eq!(m, "unknown table s.t (the catalog has t)"),
        e => panic!("expected a schema refusal, got {e}"),
    }
    let src = "create table s.t (a INTEGER);\nSELECT a FROM s.t;\nSELECT a FROM s.t WHERE a > 0;";
    match refusal(src, CatalogSource::Declared) {
        FrontendError::Schema(m) => assert_eq!(m, "unknown table t (the catalog has s.t)"),
        e => panic!("expected a schema refusal, got {e}"),
    }
    // No table of that last name, no hint.
    let src = "create table t (a INTEGER);\nSELECT a FROM t;\nSELECT a FROM u;";
    match refusal(src, CatalogSource::Declared) {
        FrontendError::Schema(m) => assert_eq!(m, "unknown table u"),
        e => panic!("expected a schema refusal, got {e}"),
    }
}
