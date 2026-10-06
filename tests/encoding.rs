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

fn is_nullary(n: &Value) -> bool {
    n["operand"].as_array().is_some_and(Vec::is_empty) && n.get("query").is_none()
}

const NUM: &str = r#"create table "t" ("a" INTEGER, "x" NUMERIC, "s" VARCHAR, "d" DATE);"#;

/// The one constant `SELECT <lit> FROM t` projects, as lowered.
fn literal(lit: &str) -> Value {
    let q = format!(r#"SELECT {lit} FROM "t""#);
    let v = lower(NUM, &q, &q);
    v["queries"][0]["project"]["target"][0].clone()
}

fn constant(text: &str, ty: &str) -> Value {
    json!({ "operator": text, "operand": [], "type": ty })
}

fn opaque_numeric(text: &str) -> Value {
    json!({ "operator": "q_numeric", "operand": [constant(text, "VARCHAR")], "type": "REAL" })
}

mod null_spelled_strings {
    use super::*;

    /// No nullary node is spelled like NULL unless it is SQL NULL: QED reads every such spelling as
    /// NULL, `sqleq-solver` the upper-case one.
    fn null_spellings(v: &Value) -> Vec<String> {
        let mut out = Vec::new();
        nodes(v, &mut out);
        out.into_iter()
            .filter(|n| is_nullary(n) && n["type"] == "VARCHAR")
            .filter_map(|n| n["operator"].as_str())
            .filter(|o| o.to_lowercase() == "null")
            .map(str::to_string)
            .collect()
    }

    #[test]
    fn a_string_spelled_null_is_not_a_null_constant() {
        for lit in ["'NULL'", "'null'", "'Null'", "'nUlL'"] {
            let q = format!(r#"SELECT "s" FROM "t" WHERE "s" = {lit} OR {lit} IS NULL"#);
            let v = lower(NUM, &q, &q);
            assert_eq!(null_spellings(&v), Vec::<String>::new(), "{lit}: {v}");
        }
    }

    #[test]
    fn it_is_the_same_string_spelled_as_a_concatenation() {
        assert_eq!(
            literal("'NULL'"),
            json!({ "operator": "||", "operand": [constant("N", "VARCHAR"), constant("ULL", "VARCHAR")], "type": "VARCHAR" })
        );
        // Two spellings are two strings, and neither is a NULL.
        assert_ne!(literal("'NULL'"), literal("'null'"));
        assert_ne!(literal("'null'"), literal("NULL"));
        // Every other string, a longer one with `null` in it included, is a plain constant.
        assert_eq!(literal("'nullable'"), constant("nullable", "VARCHAR"));
        assert_eq!(literal("''"), constant("", "VARCHAR"));
        // And SQL NULL is still the nullary NULL.
        assert_eq!(literal("NULL"), constant("NULL", "INTEGER"));
    }

    #[test]
    fn a_coerced_null_string_is_converted_not_relabelled() {
        // `d = 'NULL'` compares a date with the string's conversion. Relabelling the string as the
        // date NULL instead would make the comparison never hold.
        let q = r#"SELECT "d" FROM "t" WHERE "d" = 'NULL'"#;
        let v = lower(NUM, q, q);
        let mut out = Vec::new();
        nodes(&v["queries"][0], &mut out);
        assert!(out.iter().any(|n| n["operator"] == "q_conv_varchar_date"), "{v}");
        assert!(!out.iter().any(|n| is_nullary(n) && n["operator"] == "NULL"), "{v}");
    }
}

mod numeric_literals {
    use super::*;

    #[test]
    fn an_exponent_or_out_of_range_literal_is_numeric() {
        // Postgres types both `numeric`; they were INTEGER, and `sqleq-solver` read them as 0 and
        // as i64::MAX.
        assert_eq!(literal("1e-5"), opaque_numeric("0.00001"));
        assert_eq!(literal("9223372036854775808"), opaque_numeric("9223372036854775808"));
        assert_eq!(literal("1E+1"), constant("10", "REAL"));
        // The largest bigint is still an integer.
        assert_eq!(literal("9223372036854775807"), constant("9223372036854775807", "INTEGER"));
    }

    #[test]
    fn a_decimal_qed_would_round_is_opaque() {
        // As `f32`s, `20000000.5` is `20000000` and `0.100000001` is `0.1`.
        assert_eq!(literal("20000000.5"), opaque_numeric("20000000.5"));
        assert_eq!(literal("0.1"), opaque_numeric("0.1"));
        assert_eq!(literal("0.100000001"), opaque_numeric("0.100000001"));
        // Their `f32`'s denominator, and numerator, do not fit an `i32`: QED panicked on these.
        assert_eq!(literal("0.00001"), opaque_numeric("0.00001"));
        assert_eq!(literal("3000000000.5"), opaque_numeric("3000000000.5"));
        // Two different decimals are two different terms.
        assert_ne!(literal("20000000.5"), literal("20000000.0"));
    }

    #[test]
    fn a_decimal_qed_reads_exactly_stays_a_constant() {
        assert_eq!(literal("0.5"), constant("0.5", "REAL"));
        assert_eq!(literal("20000000.0"), constant("20000000.0", "REAL"));
        assert_eq!(literal("8388607.5"), constant("8388607.5", "REAL")); // (2^24 - 1) / 2
        assert_eq!(literal("16777216.0"), constant("16777216.0", "REAL")); // 2^24
        assert_eq!(literal("1073741824.0"), constant("1073741824.0", "REAL")); // 2^30
        assert_eq!(literal("0.000000000931322574615478515625"), constant("0.000000000931322574615478515625", "REAL")); // 2^-30
        assert_eq!(literal("0.0"), constant("0.0", "REAL"));
        // Just past each bound.
        assert_eq!(literal("8388608.5"), opaque_numeric("8388608.5")); // (2^24 + 1) / 2
        assert_eq!(literal("16777217.0"), opaque_numeric("16777217.0")); // 2^24 + 1
        assert_eq!(literal("2147483648.0"), opaque_numeric("2147483648.0")); // 2^31
        assert_eq!(
            literal("0.0000000004656612873077392578125"),
            opaque_numeric("0.0000000004656612873077392578125")
        ); // 2^-31
    }

    #[test]
    fn the_text_is_the_one_postgres_prints() {
        // QED reads a constant cast to text as its spelling, so the spelling is Postgres's output.
        assert_eq!(literal(".5"), constant("0.5", "REAL"));
        assert_eq!(literal("5."), constant("5", "REAL"));
        assert_eq!(literal("1.50e1"), constant("15.0", "REAL"));
        assert_eq!(literal("00.250"), constant("0.250", "REAL"));
        assert_eq!(literal("007"), constant("7", "INTEGER"));
        assert_eq!(literal("1_000"), constant("1000", "INTEGER"));
        assert_eq!(literal("1_000.5"), constant("1000.5", "REAL"));
        // A scale is part of a numeric's value as text: `0.10` and `0.1` stay apart.
        assert_ne!(literal("0.10"), literal("0.1"));
    }

    #[test]
    fn an_exponent_past_a_thousand_is_refused() {
        let src = format!("{NUM}\nSELECT 1e1001 FROM \"t\";\nSELECT 1 FROM \"t\";");
        assert!(refusal(&src, CatalogSource::Declared).contains("numeric literal"));
        literal("1e1000");
    }

    #[test]
    fn a_string_qed_would_round_is_not_cast_as_a_constant() {
        // Explicitly and by coercion, a string becomes a REAL through QED's parse of its text.
        let conv = |text: &str| {
            json!({ "operator": "q_conv_varchar_real", "operand": [constant(text, "VARCHAR")], "type": "REAL" })
        };
        let cast = |text: &str| json!({ "operator": "CAST", "operand": [constant(text, "VARCHAR")], "type": "REAL" });
        assert_eq!(literal("CAST('20000000.5' AS NUMERIC)"), conv("20000000.5"));
        assert_eq!(literal("'0.00001'::numeric"), conv("0.00001"));
        assert_eq!(literal("'NaN'::numeric"), conv("NaN"));
        assert_eq!(literal("'1.5'::numeric"), cast("1.5"));
        assert_eq!(literal("'-1.5'::numeric"), cast("-1.5"));
        // A NULL is still a NULL, and a number becomes text through its Postgres spelling.
        assert_eq!(literal("CAST(NULL AS NUMERIC)")["operand"][0], constant("NULL", "INTEGER"));
        assert_eq!(literal("CAST(.5 AS TEXT)"), json!({ "operator": "CAST", "operand": [constant("0.5", "REAL")], "type": "VARCHAR" }));
        let q = r#"SELECT "x" FROM "t" WHERE "x" = '20000000.5'"#;
        let v = lower(NUM, q, q);
        assert_eq!(v["queries"][0]["project"]["source"]["filter"]["condition"]["operand"][1], conv("20000000.5"), "{v}");
    }
}

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

mod depth {
    use super::*;

    const T: &str = r#"create table "t" ("id" INTEGER, "a" INTEGER, "c" VARCHAR, unique ("id"));"#;

    /// Runs `f` on a thread with room for an unoptimised build's frames.
    fn with_stack(f: impl FnOnce() + Send + 'static) {
        std::thread::Builder::new()
            .stack_size(256 << 20)
            .spawn(f)
            .expect("spawns")
            .join()
            .unwrap_or_else(|e| std::panic::resume_unwind(e));
    }

    fn chain(op: &str, operand: &str, terms: usize) -> String {
        vec![operand; terms].join(op)
    }

    #[test]
    fn a_long_arithmetic_chain_is_refused() {
        with_stack(|| {
            for (op, operand) in [(" + ", r#""a""#), (" || ", r#""c""#)] {
                for terms in [1026, 6_000, 100_000] {
                    let src = format!("{T}\nSELECT \"id\", {} AS \"x\" FROM \"t\";\nSELECT \"id\" FROM \"t\";", chain(op, operand, terms));
                    let m = refusal(&src, CatalogSource::Declared);
                    assert!(m.contains("nested more than"), "{terms} terms of {op}: {m}");
                }
            }
        });
    }

    #[test]
    fn a_long_set_operation_chain_is_refused() {
        with_stack(|| {
            for terms in [1026, 30_000] {
                let src = format!("{T}\n{};\nSELECT \"a\" FROM \"t\";", chain(" UNION ALL ", r#"SELECT "a" FROM "t""#, terms));
                let m = refusal(&src, CatalogSource::Declared);
                assert!(m.contains("nested more than"), "{terms} branches: {m}");
            }
        });
    }

    #[test]
    fn a_chain_within_the_limit_lowers() {
        with_stack(|| {
            let src = format!("{T}\nSELECT \"id\", {} AS \"x\" FROM \"t\";\nSELECT \"id\" FROM \"t\";", chain(" + ", r#""a""#, 1000));
            lower_in(&src, CatalogSource::Declared);
            let src = format!("{T}\n{};\nSELECT \"a\" FROM \"t\";", chain(" UNION ALL ", r#"SELECT "a" FROM "t""#, 1000));
            lower_in(&src, CatalogSource::Declared);
        });
    }
}
