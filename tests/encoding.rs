// Unless explicitly stated otherwise all files in this repository are licensed under the
// Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/).
// Copyright 2026-Present Datadog, Inc.

//! What the IR says about constants, operators, keys and query-level clauses, read the way the
//! provers read it.
//!
//! A prover reads a constant's value off its name, `/` and `%` off its own arithmetic, and a key
//! as "two rows agreeing on these columns are one row". Each module below pins a lowering that used
//! to say more than Postgres does, or an input that used to crash the frontend. They run no prover.

use serde_json::{json, Value};
use sqleq_frontend::{lower_with, CatalogSource, FrontendError};

fn lower_in(src: &str, mode: CatalogSource) -> Value {
    lower_with(src, mode).unwrap_or_else(|e| panic!("expected Ok, got {e}\n{src}"))
}

/// The pair lowered against `ddl`, declared catalog.
fn lower(ddl: &str, q0: &str, q1: &str) -> Value {
    lower_in(&format!("{ddl}\n{q0};\n{q1};"), CatalogSource::Declared)
}

fn refusal(src: &str, mode: CatalogSource) -> String {
    match lower_with(src, mode) {
        Err(FrontendError::Unsupported(m)) => m,
        Err(e) => panic!("expected an Unsupported refusal, got {e}\n{src}"),
        Ok(_) => panic!("expected a refusal, but it lowered\n{src}"),
    }
}

/// Every node anywhere in `v` that has an `"operator"`.
fn nodes<'a>(v: &'a Value, out: &mut Vec<&'a Value>) {
    match v {
        Value::Object(m) => {
            if m.contains_key("operator") {
                out.push(v);
            }
            m.values().for_each(|x| nodes(x, out));
        }
        Value::Array(a) => a.iter().for_each(|x| nodes(x, out)),
        _ => {}
    }
}

const NUM: &str = r#"create table "t" ("a" INTEGER, "x" NUMERIC, "s" VARCHAR, "d" DATE);"#;

mod integer_division {
    use super::*;

    fn target(q: &str) -> Value {
        let v = lower(NUM, q, q);
        v["queries"][0]["project"]["target"][0].clone()
    }

    #[test]
    fn integer_division_and_modulo_are_functions() {
        let div = target(r#"SELECT "a" / 2 FROM "t""#);
        assert_eq!((div["operator"].as_str(), div["type"].as_str()), (Some("q_arith_div_integer_integer"), Some("INTEGER")));
        let rem = target(r#"SELECT "a" % 2 FROM "t""#);
        assert_eq!((rem["operator"].as_str(), rem["type"].as_str()), (Some("q_arith_mod_integer_integer"), Some("INTEGER")));
        let lit = target(r#"SELECT (-7) / 2 FROM "t""#);
        assert_eq!(lit["operator"], "q_arith_div_integer_integer");
        // Over aggregates too.
        let q = r#"SELECT sum("a") / count(*) FROM "t""#;
        let v = lower(NUM, q, q);
        let mut out = Vec::new();
        nodes(&v["queries"][0], &mut out);
        assert!(out.iter().any(|n| n["operator"] == "q_arith_div_integer_integer"), "{v}");
        assert!(!out.iter().any(|n| n["operator"] == "/"), "{v}");
    }

    #[test]
    fn other_arithmetic_stays_native() {
        assert_eq!(target(r#"SELECT "x" / 2 FROM "t""#)["operator"], "/");
        assert_eq!(target(r#"SELECT "a" / 2.0 FROM "t""#)["operator"], "/");
        assert_eq!(target(r#"SELECT "a" * 2 FROM "t""#)["operator"], "*");
    }
}

mod keys {
    use super::*;

    fn keys(ddl: &str) -> Value {
        lower(ddl, r#"SELECT 1 FROM "t""#, r#"SELECT 1 FROM "t""#)["schemas"][0]["key"].clone()
    }

    #[test]
    fn a_unique_column_that_may_be_null_is_not_a_key() {
        assert_eq!(keys(r#"create table "t" ("u" INTEGER, unique ("u"));"#), json!([]));
        assert_eq!(keys(r#"create table "t" ("u" INTEGER UNIQUE);"#), json!([]));
        assert_eq!(keys(r#"create table "t" ("u" INTEGER NOT NULL, "v" INTEGER, unique ("u", "v"));"#), json!([]));
    }

    #[test]
    fn a_key_postgres_enforces_on_every_row_is_kept() {
        assert_eq!(keys(r#"create table "t" ("u" INTEGER NOT NULL, unique ("u"));"#), json!([[0]]));
        assert_eq!(keys(r#"create table "t" ("u" INTEGER PRIMARY KEY);"#), json!([[0]]));
        assert_eq!(keys(r#"create table "t" ("u" INTEGER, "v" INTEGER, primary key ("u", "v"));"#), json!([[0, 1]]));
        assert_eq!(
            keys(r#"create table "t" ("u" INTEGER NOT NULL, "v" INTEGER, unique ("u"), unique ("v"));"#),
            json!([[0]])
        );
    }
}

mod surviving_with {
    use super::*;

    const T: &str = r#"create table "t" ("a" INTEGER); create table "u" ("a" INTEGER);"#;

    fn refused(q0: &str, q1: &str, mode: CatalogSource) {
        let m = refusal(&format!("{T}\n{q0};\n{q1};"), mode);
        assert!(m.contains("WITH"), "{m}");
    }

    #[test]
    fn a_recursive_with_is_refused_at_every_level() {
        let rec = r#"WITH RECURSIVE "t" ("a") AS (SELECT 1 UNION ALL SELECT "a" + 1 FROM "t" WHERE "a" < 3) SELECT "a" FROM "t""#;
        refused(rec, r#"SELECT "a" FROM "t""#, CatalogSource::Declared);
        let nested = r#"SELECT "s"."a" FROM (WITH RECURSIVE "t" AS (SELECT 1 AS "a") SELECT "a" FROM "t") AS "s""#;
        for mode in [CatalogSource::Declared, CatalogSource::InferredSeeded, CatalogSource::Inferred] {
            refused(nested, r#"SELECT "s"."a" FROM (SELECT "a" FROM "t") AS "s""#, mode);
        }
        let sub = r#"SELECT "a" FROM "u" WHERE "a" IN (WITH RECURSIVE "t" AS (SELECT 1 AS "a") SELECT "a" FROM "t")"#;
        refused(sub, r#"SELECT "a" FROM "u" WHERE "a" IN (SELECT "a" FROM "t")"#, CatalogSource::Declared);
    }

    #[test]
    fn a_data_modifying_with_is_refused_read_or_not() {
        refused(
            r#"WITH "t" AS (DELETE FROM "u" RETURNING "a") SELECT "a" FROM "t""#,
            r#"SELECT "a" FROM "t""#,
            CatalogSource::Declared,
        );
        // Never read, its effect is still there: it empties `u`.
        refused(
            r#"WITH "d" AS (DELETE FROM "u" RETURNING "a") SELECT "a" FROM "t""#,
            r#"SELECT "a" FROM "t""#,
            CatalogSource::Declared,
        );
    }

    #[test]
    fn a_with_that_inlines_still_lowers() {
        let v = lower(T, r#"WITH "c" AS (SELECT "a" FROM "u") SELECT "a" FROM "c""#, r#"SELECT "a" FROM (SELECT "a" FROM "u") AS "c""#);
        assert_eq!(v["queries"][0], v["queries"][1]);
    }
}