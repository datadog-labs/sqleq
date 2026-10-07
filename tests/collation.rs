// Unless explicitly stated otherwise all files in this repository are licensed under the
// Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/).
// Copyright 2026-Present Datadog, Inc.

//! String order and collations.
//!
//! * An order comparison of two strings is native only under `C` or `POSIX`, named by a column's
//!   `COLLATE` or an operand's; under the database's default collation, which no input states, it is
//!   the uninterpreted `q_str_lt`/`q_str_le`, and under a named one the same with the collation's name
//!   as a third operand.
//! * A column under a collation that may be non-deterministic, or that no predicate can name, is
//!   refused wherever a query reads it.
//! * Where a column the pair reads declares a collation, an operation that reads one its symbol does
//!   not name is refused, and a shared `ORDER BY ... LIMIT` is not stripped.
//!
//! Each "refused" test is a pair that is **not** equivalent in Postgres (under some database default
//! collation) and used to lower to IR a prover proves equal. They do not run a prover; they pin the
//! lowering.

use serde_json::Value;
use sqleq_frontend::{lower_with, lower_with_ddl, CatalogSource, FrontendError};

const MODES: [CatalogSource; 2] = [CatalogSource::Declared, CatalogSource::InferredSeeded];

fn lower_in(ddl: &str, q0: &str, q1: &str, src: CatalogSource) -> Value {
    lower_with(&format!("{ddl}\n{q0};\n{q1};"), src).unwrap_or_else(|e| panic!("{src:?}: expected Ok, got {e}"))
}

/// The pair lowered in every mode that reads the declared schema, which must agree.
fn lower(ddl: &str, q0: &str, q1: &str) -> Value {
    let [a, b] = MODES.map(|src| lower_in(ddl, q0, q1, src));
    assert_eq!(a["queries"], b["queries"], "the two catalog modes disagree on {q0} / {q1}");
    a
}

fn refused(ddl: &str, q0: &str, q1: &str, needle: &str) {
    for src in MODES {
        match lower_with(&format!("{ddl}\n{q0};\n{q1};"), src) {
            Err(FrontendError::Unsupported(m)) => assert!(m.contains(needle), "{src:?}: refused for {m:?}"),
            Err(e) => panic!("{src:?}: expected an Unsupported refusal mentioning {needle:?}, got {e}"),
            Ok(_) => panic!("{src:?}: expected a refusal mentioning {needle:?}, but it lowered"),
        }
    }
}

/// Every node with an operator in the pair's queries, as `(operator, operand count)`.
fn nodes(v: &Value, out: &mut Vec<(String, usize)>) {
    match v {
        Value::Object(m) => {
            if let Some(Value::String(o)) = m.get("operator") {
                out.push((o.clone(), m.get("operand").and_then(Value::as_array).map_or(0, Vec::len)));
            }
            m.values().for_each(|x| nodes(x, out));
        }
        Value::Array(a) => a.iter().for_each(|x| nodes(x, out)),
        _ => {}
    }
}

fn ops(v: &Value) -> Vec<(String, usize)> {
    let mut out = Vec::new();
    nodes(&v["queries"], &mut out);
    out
}

fn has(v: &Value, op: &str, arity: usize) -> bool {
    ops(v).iter().any(|(o, n)| o == op && *n == arity)
}

fn native_order(v: &Value) -> bool {
    ops(v).iter().any(|(o, _)| matches!(o.as_str(), "<" | "<=" | ">" | ">="))
}

/// The first node named `op`, depth first.
fn find<'a>(v: &'a Value, op: &str) -> Option<&'a Value> {
    match v {
        Value::Object(m) => {
            if m.get("operator").and_then(Value::as_str) == Some(op) {
                return Some(v);
            }
            m.values().find_map(|x| find(x, op))
        }
        Value::Array(a) => a.iter().find_map(|x| find(x, op)),
        _ => None,
    }
}

const T: &str = r#"create table "t" ("id" INTEGER, "s" TEXT, "c" TEXT COLLATE "C", "e" TEXT COLLATE "en_US.utf8");"#;
const DEFAULT_ONLY: &str = r#"create table "t" ("id" INTEGER, "s" TEXT, "v" VARCHAR(20));"#;

#[test]
fn string_order_under_the_default_collation_is_uninterpreted() {
    // Under `en_US.utf8`, 'a' < 'b' < 'B': `s > 'a' AND s < 'B'` holds of 'b', and decided by code
    // point it holds of nothing.
    let v = lower(DEFAULT_ONLY, r#"SELECT "id" FROM "t" WHERE "s" > 'a' AND "s" < 'B'"#, r#"SELECT "id" FROM "t" WHERE FALSE"#);
    assert!(!native_order(&v), "{v}");
    assert!(has(&v, "q_str_lt", 2), "{v}");
    // Two constants are ordered by the database's default collation too.
    // (Declared only: inference has no column to type here.)
    let v = lower_in(DEFAULT_ONLY, r#"SELECT CASE WHEN 'a' < 'B' THEN 1 ELSE 0 END FROM "t""#, r#"SELECT 0 FROM "t""#, CatalogSource::Declared);
    assert!(!native_order(&v) && has(&v, "q_str_lt", 2), "{v}");
    // `<=`, `>=` and BETWEEN, and a `varchar(n)` column, alike.
    let v = lower(
        DEFAULT_ONLY,
        r#"SELECT "id" FROM "t" WHERE "s" BETWEEN 'A' AND 'a'"#,
        r#"SELECT "id" FROM "t" WHERE "v" <= 'a' OR "v" >= 'b'"#,
    );
    assert!(!native_order(&v) && has(&v, "q_str_le", 2), "{v}");
    assert!(!ops(&v).iter().any(|(o, _)| o == "q_str_lt"), "{v}");
}

#[test]
fn greater_is_less_with_the_operands_swapped() {
    let v = lower(
        DEFAULT_ONLY,
        r#"SELECT "id" FROM "t" WHERE "s" > 'b' AND "s" >= 'a'"#,
        r#"SELECT "id" FROM "t" WHERE 'b' < "s" AND 'a' <= "s""#,
    );
    assert_eq!(v["queries"][0], v["queries"][1], "{v}");
    let lt = find(&v["queries"][0], "q_str_lt").expect("a q_str_lt");
    assert_eq!(lt["operand"][0]["operator"], "b", "{lt}");
    assert_eq!(lt["operand"][1]["column"], 1, "{lt}");
}

#[test]
fn order_of_other_types_stays_native() {
    let v = lower(DEFAULT_ONLY, r#"SELECT "id" FROM "t" WHERE "id" < 3"#, r#"SELECT "id" FROM "t" WHERE "id" <= 2"#);
    assert!(native_order(&v), "{v}");
    assert!(!ops(&v).iter().any(|(o, _)| o.starts_with("q_str_")), "{v}");
}

#[test]
fn c_and_posix_order_by_code_point_natively() {
    let ddl = r#"create table "t" ("id" INTEGER, "c" TEXT COLLATE "C", "p" VARCHAR(9) COLLATE "POSIX", "q" TEXT COLLATE pg_catalog."C");"#;
    for col in ["c", "p", "q"] {
        let v = lower(ddl, &format!(r#"SELECT "id" FROM "t" WHERE "{col}" = 'B' AND "{col}" < 'a'"#), r#"SELECT "id" FROM "t""#);
        assert!(native_order(&v), "{col}: {v}");
        assert!(!ops(&v).iter().any(|(o, _)| o.starts_with("q_str_")), "{col}: {v}");
    }
    // A column's collation beats the default of a constant, and of a column that declares none.
    let v = lower(T, r#"SELECT "id" FROM "t" WHERE "c" < "s" AND 'a' > "c""#, r#"SELECT "id" FROM "t""#);
    assert!(native_order(&v) && !has(&v, "q_str_lt", 2), "{v}");
}

#[test]
fn an_operand_may_name_c_or_posix() {
    for q in [
        r#"SELECT "id" FROM "t" WHERE "s" = 'B' AND "s" < 'a' COLLATE "C""#,
        r#"SELECT "id" FROM "t" WHERE "s" = 'B' AND ("s" COLLATE "POSIX") < 'a'"#,
        r#"SELECT "id" FROM "t" WHERE "s" = 'B' AND "s" COLLATE pg_catalog."C" BETWEEN 'A' AND 'a'"#,
    ] {
        let v = lower(DEFAULT_ONLY, q, r#"SELECT "id" FROM "t" WHERE "s" = 'B'"#);
        assert!(native_order(&v), "{q}: {v}");
        assert!(!ops(&v).iter().any(|(o, _)| o.starts_with("q_str_")), "{q}: {v}");
    }
    // Any other collation an operand names is still refused, and so is a `COLLATE` that is not an
    // operand of a comparison: `upper` reads it.
    refused(DEFAULT_ONLY, r#"SELECT "id" FROM "t" WHERE "s" < 'a' COLLATE "en_US.utf8""#, r#"SELECT "id" FROM "t""#, "COLLATE");
    refused(DEFAULT_ONLY, r#"SELECT upper("s" COLLATE "C") FROM "t""#, r#"SELECT upper("s") FROM "t""#, "Collate");
    // Unquoted, `C` folds to `c`, which is not a collation Postgres has.
    refused(DEFAULT_ONLY, r#"SELECT "id" FROM "t" WHERE "s" < 'a' COLLATE C"#, r#"SELECT "id" FROM "t""#, "COLLATE");
}

#[test]
fn a_named_collation_is_a_predicate_of_its_own() {
    let ddl = r#"create table "t" ("id" INTEGER, "e" TEXT COLLATE "en_US.utf8", "g" TEXT COLLATE "de_DE.utf8", "i" TEXT COLLATE "und-x-icu", "s" TEXT);"#;
    let v = lower(ddl, r#"SELECT "id" FROM "t" WHERE "e" = 'B' AND "e" < 'a'"#, r#"SELECT "id" FROM "t" WHERE "e" = 'B'"#);
    assert!(!native_order(&v), "{v}");
    let lt = find(&v["queries"][0], "q_str_lt").expect("a q_str_lt");
    assert_eq!(lt["operand"][2]["operator"], "en_US.utf8", "{lt}");
    // Two collations, two predicates, even over one value.
    let v = lower(
        ddl,
        r#"SELECT "id" FROM "t" WHERE "e" = "s" AND "e" < 'a'"#,
        r#"SELECT "id" FROM "t" WHERE "g" = "s" AND "g" < 'a'"#,
    );
    let names = |q: &Value| find(q, "q_str_lt").map(|n| n["operand"][2]["operator"].clone());
    assert_eq!(names(&v["queries"][0]), Some("en_US.utf8".into()));
    assert_eq!(names(&v["queries"][1]), Some("de_DE.utf8".into()));
    let v = lower(ddl, r#"SELECT "id" FROM "t" WHERE "i" < 'a'"#, r#"SELECT "id" FROM "t""#);
    assert!(has(&v, "q_str_lt", 3), "{v}");
}

#[test]
fn the_default_collation_spelled_out_is_the_default() {
    let ddl = r#"create table "t" ("id" INTEGER, "s" TEXT COLLATE pg_catalog."default", "d" TEXT COLLATE "default");"#;
    let v = lower(ddl, r#"SELECT MAX("s") FROM "t" WHERE "d" < 'a'"#, r#"SELECT MAX("d") FROM "t" WHERE "s" < 'a'"#);
    assert!(has(&v, "q_str_lt", 2) && !has(&v, "q_str_lt", 3), "{v}");
}

#[test]
fn two_collations_in_one_comparison_are_refused() {
    // Postgres raises an error: neither column's collation wins.
    refused(T, r#"SELECT "id" FROM "t" WHERE "c" < "e""#, r#"SELECT "id" FROM "t""#, "two collations");
}

#[test]
fn a_collation_that_may_be_non_deterministic_is_refused() {
    // Under `ci`, 'a' = 'A': `s = 'A' AND NOT s = 'a'` holds of nothing, and read as identity it
    // holds of 'A'.
    let ci = r#"create collation "ci" (provider = icu, locale = 'und-u-ks-level2', deterministic = false);"#;
    let t = r#"create table "t" ("id" INTEGER, "s" TEXT COLLATE "ci");"#;
    let (q0, q1) = (r#"SELECT "id" FROM "t" WHERE "s" = 'A' AND NOT "s" = 'a'"#, r#"SELECT "id" FROM "t" WHERE "s" = 'A'"#);
    refused(&format!("{ci}\n{t}"), q0, q1, "collation");
    // Created or not: a name the DDL does not create and Postgres does not predefine may be either.
    refused(t, q0, q1, "collation");
    refused(r#"create table "t" ("id" INTEGER, "s" TEXT COLLATE "public"."en_US.utf8");"#, q0, q1, "collation");
    // A column no query reads costs nothing; nor does a pair whose two queries lower to one plan.
    lower(&format!("{ci}\n{t}"), r#"SELECT "id" FROM "t""#, r#"SELECT "id" + 0 FROM "t""#);
    lower(t, q1, q1);
    // A collation the DDL creates deterministic is a named collation, and so is a copy of a
    // predefined one; one created twice, or under a predefined name, is refused.
    for create in [
        r#"create collation "fr" (provider = icu, locale = 'fr', deterministic = true);"#,
        r#"create collation "fr" (provider = icu, locale = 'fr');"#,
        r#"create collation "fr" from "en_US.utf8";"#,
    ] {
        let v = lower(&format!("{create}\ncreate table \"t\" (\"id\" INTEGER, \"s\" TEXT COLLATE \"fr\");"), q0, q1);
        assert!(!native_order(&v), "{create}: {v}");
    }
    for (create, collation) in [
        (r#"create collation "fr" (locale = 'fr'); create collation other."fr" (locale = 'fr');"#, "fr"),
        (r#"create collation "en_US.utf8" (provider = icu, locale = 'en', deterministic = false);"#, "en_US.utf8"),
        (r#"create collation "en_US.utf8" (provider = icu, locale = 'en');"#, "en_US.utf8"),
        (r#"create collation "fr" (locale = 'fr', deterministic = 'no');"#, "fr"),
        (r#"create collation "fr" from "ci";"#, "fr"),
    ] {
        let t = format!("{create}\ncreate table \"t\" (\"id\" INTEGER, \"s\" TEXT COLLATE \"{collation}\");");
        refused(&t, q0, q1, "collation");
    }
}

#[test]
fn the_raw_ddl_reader_reads_collate_too() {
    let ddl = r#"CREATE COLLATION public.ci (provider = icu, locale = 'und-u-ks-level2', deterministic = false);
CREATE TABLE public.t (id integer, s text COLLATE public.ci, e character varying(9) COLLATE "en_US.utf8", c text COLLATE "C");"#;
    let pair = |q0: &str, q1: &str| format!("{q0};\n{q1};");
    for src in MODES {
        let refusal = lower_with_ddl(&pair(r#"SELECT id FROM t WHERE s = 'A' AND NOT s = 'a'"#, r#"SELECT id FROM t WHERE s = 'A'"#), ddl, src);
        assert!(matches!(refusal, Err(FrontendError::Unsupported(ref m)) if m.contains("collation")), "{src:?}: {refusal:?}");
        let v = lower_with_ddl(&pair(r#"SELECT id FROM t WHERE e < 'a' AND c < 'a'"#, r#"SELECT id FROM t"#), ddl, src)
            .unwrap_or_else(|e| panic!("{src:?}: {e}"));
        assert!(has(&v, "q_str_lt", 3), "{src:?}: {v}");
        assert!(native_order(&v), "{src:?}: {v}");
    }
}

#[test]
fn an_opaque_column_under_a_declared_collation_is_refused() {
    // An array of text orders by its elements' collation, which nothing names on the opaque sort.
    let ddl = r#"create table "t" ("id" INTEGER, "a" TEXT[] COLLATE "C", "b" TEXT[]);"#;
    refused(ddl, r#"SELECT "id" FROM "t" WHERE "a" = "b" AND "a" < "b""#, r#"SELECT "id" FROM "t" WHERE FALSE"#, "collation");
}

#[test]
fn where_a_column_declares_a_collation_operations_that_read_one_are_refused() {
    // Each pair differs in Postgres under a database collation such as `en_US.utf8`, where `C` and the
    // default order 'a' and 'B' differently and only the default upper-cases 'é'.
    let ddl = r#"create table "t" ("id" INTEGER, "c" TEXT COLLATE "C", "d" TEXT);"#;
    refused(ddl, r#"SELECT MAX("c") FROM "t" WHERE "c" = "d""#, r#"SELECT MAX("d") FROM "t" WHERE "c" = "d""#, "MAX over a string");
    refused(ddl, r#"SELECT upper("c") FROM "t" WHERE "c" = 'é'"#, r#"SELECT upper('é') FROM "t" WHERE "c" = 'é'"#, "over a string");
    refused(
        ddl,
        r#"SELECT greatest("c", 'a') FROM "t" WHERE "c" = "d""#,
        r#"SELECT greatest("d", 'a') FROM "t" WHERE "c" = "d""#,
        "over a string",
    );
    // A shared `ORDER BY .. LIMIT` stays, rather than being stripped from both sides, and a slice
    // ordered by a string is refused.
    refused(
        ddl,
        r#"SELECT "c" AS "x" FROM "t" WHERE "c" = "d" ORDER BY "x" LIMIT 1"#,
        r#"SELECT "d" AS "x" FROM "t" WHERE "c" = "d" ORDER BY "x" LIMIT 1"#,
        "row slice ordered by a string",
    );
    // A comparison whose operand is neither a column nor a constant: `||` keeps `c`'s collation.
    refused(
        ddl,
        r#"SELECT "id" FROM "t" WHERE "c" = "d" AND ("c" || 'x') < 'b'"#,
        r#"SELECT "id" FROM "t" WHERE "c" = "d" AND ("d" || 'x') < 'b'"#,
        "collation comes from the columns it reads",
    );
    refused(
        ddl,
        r#"SELECT "c" FROM "t" GROUP BY "c" HAVING MIN("d") < 'b'"#,
        r#"SELECT "c" FROM "t" GROUP BY "c""#,
        "after grouping",
    );
    // What never reads a collation still lowers: equality, `LIKE`, `||`, a cast, a count, and the
    // comparisons whose collation is known.
    let v = lower(
        ddl,
        r#"SELECT "c" || 'x', COUNT(DISTINCT "d") FROM "t" WHERE "c" = "d" AND "d" LIKE 'a%' AND "c" < 'b' AND "d" < 'b' GROUP BY "c""#,
        r#"SELECT "c" || 'x', COUNT(DISTINCT "d") FROM "t" WHERE "d" = "c" AND "d" LIKE 'a%' AND "c" < 'b' AND "d" < 'b' GROUP BY "c""#,
    );
    assert!(native_order(&v) && has(&v, "q_str_lt", 2), "{v}");
    // The same operations over a table whose columns declare none are not refused, even where
    // another table of the schema has one.
    let two = format!(r#"{ddl} create table "u" ("id" INTEGER, "s" TEXT);"#);
    let v = lower(
        &two,
        r#"SELECT MAX(upper("s")) FROM "u" WHERE ("s" || 'x') < 'b'"#,
        r#"SELECT MAX(upper("s")) FROM "u" WHERE 'b' > ("s" || 'x')"#,
    );
    assert!(has(&v, "q_str_lt", 2), "{v}");
}

#[test]
fn an_order_comparison_after_grouping_is_uninterpreted() {
    let v = lower(
        DEFAULT_ONLY,
        r#"SELECT "s", COUNT(*) FROM "t" GROUP BY "s" HAVING MAX("s") < 'm'"#,
        r#"SELECT "s", COUNT(*) FROM "t" GROUP BY "s" HAVING 'm' > MAX("s")"#,
    );
    assert_eq!(v["queries"][0], v["queries"][1], "{v}");
    assert!(!native_order(&v) && has(&v, "q_str_lt", 2), "{v}");
}
