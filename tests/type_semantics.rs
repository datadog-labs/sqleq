// Unless explicitly stated otherwise all files in this repository are licensed under the
// Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/).
// Copyright 2026-Present Datadog, Inc.

//! Operators and types whose Postgres meaning a prover would misread, and two inputs that used to
//! panic the frontend or the QED prover.
//!
//! * `->` and `#>` (and `->>` and `#>>`) are two operations, and `json_extract_path` is neither.
//! * A string literal against an INTEGER or BOOLEAN is read at that type, as Postgres reads it, not
//!   compared with the other side cast to text.
//! * `citext` and `char(n)` compare in a way no prover's `=` does, and are refused; floats round and
//!   are opaque; `numeric` division rounds and is uninterpreted; `int4range` and `point` are not
//!   integers. Both type readers agree on every name.
//! * `parse_declare` reads a line with non-ASCII letters in it without panicking or misreading it.
//! * `x IN (SELECT ..)` reaches the prover with the operand and the column of one type, or is refused.
//!
//! Each "not identical" or "refused" test is a pair that is **not** equivalent in Postgres and used to
//! lower to IR a prover proves equal. They do not run a prover; they pin the lowering.

use serde_json::Value;
use sqleq_frontend::{lower_sql, lower_with, lower_with_ddl, CatalogSource, FrontendError};

const T: &str = r#"create table "t" ("id" INTEGER, "a" INTEGER, "p" BOOLEAN, "s" VARCHAR, "j" JSONB, "n" NUMERIC, unique ("id"));"#;

fn lower_in(ddl: &str, q0: &str, q1: &str, src: CatalogSource) -> Value {
    lower_with(&format!("{ddl}\n{q0};\n{q1};"), src).unwrap_or_else(|e| panic!("expected Ok, got {e}"))
}

fn lower(q0: &str, q1: &str) -> Value {
    lower_in(T, q0, q1, CatalogSource::Declared)
}

/// Whether the pair lowers to byte-identical queries, in both modes that read the declared schema.
fn identical(q0: &str, q1: &str) -> bool {
    let both: Vec<bool> = [CatalogSource::Declared, CatalogSource::InferredSeeded]
        .into_iter()
        .map(|src| {
            let v = lower_in(T, q0, q1, src);
            v["queries"][0] == v["queries"][1]
        })
        .collect();
    assert_eq!(both[0], both[1], "the two catalog modes disagree on {q0} / {q1}");
    both[0]
}

fn operators(v: &Value, out: &mut Vec<String>) {
    match v {
        Value::Object(m) => {
            if let Some(Value::String(o)) = m.get("operator") {
                out.push(o.clone());
            }
            m.values().for_each(|x| operators(x, out));
        }
        Value::Array(a) => a.iter().for_each(|x| operators(x, out)),
        _ => {}
    }
}

fn ops(v: &Value) -> Vec<String> {
    let mut out = Vec::new();
    operators(&v["queries"], &mut out);
    out
}

fn refused_in(ddl: &str, q0: &str, q1: &str, src: CatalogSource, needle: &str) {
    match lower_with(&format!("{ddl}\n{q0};\n{q1};"), src) {
        Err(FrontendError::Unsupported(m)) => assert!(m.contains(needle), "{src:?}: refused for {m:?}"),
        Err(e) => panic!("{src:?}: expected an Unsupported refusal mentioning {needle:?}, got {e}"),
        Ok(_) => panic!("{src:?}: expected a refusal mentioning {needle:?}, but it lowered"),
    }
}

/// Refused in every catalog mode that reads the declared schema.
fn refused(ddl: &str, q0: &str, q1: &str, needle: &str) {
    for src in [CatalogSource::Declared, CatalogSource::InferredSeeded] {
        refused_in(ddl, q0, q1, src, needle);
    }
}

/// The schema types the frontend gives one table's columns.
fn schema_types(v: &Value) -> Vec<String> {
    v["schemas"][0]["types"].as_array().unwrap().iter().map(|t| t.as_str().unwrap().to_string()).collect()
}

// ---------------------------------------------------------------------------------------------------
// #58 (1): `->` and `#>` share one symbol
// ---------------------------------------------------------------------------------------------------

#[test]
fn a_key_lookup_and_a_path_lookup_are_two_operations() {
    // Over `{"a": "x"}`, `j ->> '{a}'` looks for the key `{a}` (NULL) and `j #>> '{a}'` follows the
    // path `[a]` (`x`).
    assert!(!identical(r#"SELECT "j" ->> '{a}' FROM "t""#, r#"SELECT "j" #>> '{a}' FROM "t""#));
    assert!(!identical(
        r#"SELECT CAST("j" -> '{a}' AS text) FROM "t""#,
        r#"SELECT CAST("j" #> '{a}' AS text) FROM "t""#,
    ));
}

#[test]
fn json_extract_path_is_neither_operator() {
    // Over `[5]`, `jsonb_extract_path(j, '0')` follows the path to `5`; `j -> '0'` looks for the key
    // `0` and finds NULL.
    assert!(!identical(r#"SELECT jsonb_extract_path("j", '0') FROM "t""#, r#"SELECT "j" -> '0' FROM "t""#));
    assert!(!identical(
        r#"SELECT jsonb_extract_path_text("j", '0') FROM "t""#,
        r#"SELECT "j" ->> '0' FROM "t""#,
    ));
    // Over `{"{a}": 1, "a": 2}`, `j #> '{a}'` follows `[a]` to 2, and `jsonb_extract_path(j, '{a}')`
    // follows `["{a}"]` to 1: the operator takes the path as one array, the function one element per
    // argument, so the two spellings cannot share a symbol.
    assert!(!identical(r#"SELECT jsonb_extract_path("j", '{a}') FROM "t""#, r#"SELECT "j" #> '{a}' FROM "t""#));
    // The two spellings of the function, json and jsonb, stay one symbol.
    let v = lower(r#"SELECT jsonb_extract_path_text("j", 'a') FROM "t""#, r#"SELECT 1 FROM "t""#);
    assert!(ops(&v).iter().any(|o| o.eq_ignore_ascii_case("q_str_jsonpath_elems")), "{:?}", ops(&v));
}

#[test]
fn another_dialects_json_extract_is_an_ordinary_call() {
    // `json_extract` reads a JSONPath in the dialects that have it, and Postgres has none: it is not
    // `->`.
    assert!(!identical(r#"SELECT json_extract("j", 'a') FROM "t""#, r#"SELECT "j" -> 'a' FROM "t""#));
}

// ---------------------------------------------------------------------------------------------------
// #58 (2): a string literal against an INTEGER or BOOLEAN casts the other side to text
// ---------------------------------------------------------------------------------------------------

#[test]
fn a_string_literal_against_an_integer_is_read_as_an_integer() {
    // `'01'` is the integer 1 to Postgres. Compared as text with `a::text`, `a = '1' AND NOT a = '01'`
    // reduced to `a = '1'`, though on `t = {(1)}` the first returns no rows.
    assert!(identical(r#"SELECT "a" FROM "t" WHERE "a" = '01'"#, r#"SELECT "a" FROM "t" WHERE "a" = 1"#));
    assert!(identical(r#"SELECT "a" FROM "t" WHERE '01' = "a""#, r#"SELECT "a" FROM "t" WHERE 1 = "a""#));
    assert!(identical(r#"SELECT "a" FROM "t" WHERE "a" = ' +7 '"#, r#"SELECT "a" FROM "t" WHERE "a" = 7"#));
    assert!(identical(r#"SELECT "a" FROM "t" WHERE "a" < '10'"#, r#"SELECT "a" FROM "t" WHERE "a" < 10"#));
    assert!(identical(
        r#"SELECT "a" FROM "t" WHERE "a" IN ('1', '02')"#,
        r#"SELECT "a" FROM "t" WHERE "a" IN (1, 2)"#,
    ));
    assert!(identical(
        r#"SELECT "a" FROM "t" WHERE "a" BETWEEN '01' AND '9'"#,
        r#"SELECT "a" FROM "t" WHERE "a" BETWEEN 1 AND 9"#,
    ));
    assert!(identical(r#"SELECT "a" + '1' FROM "t""#, r#"SELECT "a" + 1 FROM "t""#));
}

#[test]
fn a_string_literal_against_a_boolean_is_read_as_a_boolean() {
    // `'t'`, `'true'`, `'yes'`, `'on'` and `'1'` are all `true` to Postgres.
    for lit in ["t", "true", "yes", "on", "1", " TrU ", "y"] {
        assert!(
            identical(&format!(r#"SELECT "a" FROM "t" WHERE "p" = '{lit}'"#), r#"SELECT "a" FROM "t" WHERE "p" = true"#),
            "{lit:?}"
        );
    }
    for lit in ["f", "false", "no", "off", "of", "0", "n"] {
        assert!(
            identical(&format!(r#"SELECT "a" FROM "t" WHERE "p" = '{lit}'"#), r#"SELECT "a" FROM "t" WHERE "p" = false"#),
            "{lit:?}"
        );
    }
}

#[test]
fn a_case_branch_literal_takes_the_other_branches_type() {
    // The CASE is an integer, so `'7'` is 7; read as text it made the whole CASE text, and its
    // comparison with `'01'` a string comparison.
    assert!(identical(
        r#"SELECT "a" FROM "t" WHERE CASE WHEN "p" THEN "a" ELSE '7' END = '01'"#,
        r#"SELECT "a" FROM "t" WHERE CASE WHEN "p" THEN "a" ELSE 7 END = 1"#,
    ));
    // With no typed branch, Postgres makes the CASE text, and so does the lowering.
    let v = lower(r#"SELECT CASE WHEN "p" THEN NULL ELSE '1' END FROM "t""#, r#"SELECT 1 FROM "t""#);
    assert_eq!(v["queries"][0]["project"]["target"][0]["type"], "VARCHAR");
}

#[test]
fn a_literal_postgres_reads_differently_stays_a_cast_of_the_literal() {
    // Text the reading does not understand is never a constant, and never makes the column text:
    // `'0x1F'` is 31 to Postgres, `'abc'` an error, `'o'` ambiguous.
    for (col, lit) in [("a", "0x1F"), ("a", "abc"), ("a", "1.0"), ("p", "o"), ("p", "truex"), ("p", "")] {
        let v = lower(&format!(r#"SELECT "id" FROM "t" WHERE "{col}" = '{lit}'"#), r#"SELECT 1 FROM "t""#);
        let cmp = &v["queries"][0]["project"]["source"]["filter"]["condition"];
        assert!(cmp["operand"][0].get("column").is_some(), "{lit:?}: the column was cast: {cmp}");
        assert_eq!(cmp["operand"][1]["operator"], "CAST", "{lit:?}: {cmp}");
        assert_eq!(cmp["operand"][1]["operand"][0]["operator"], lit, "{lit:?}: {cmp}");
    }
}

#[test]
fn a_string_literal_against_text_and_other_types_is_unchanged() {
    // Text against text stays a string comparison, and `'1.5'` against a numeric is cast to it, as
    // before.
    let v = lower(r#"SELECT "id" FROM "t" WHERE "s" = '01'"#, r#"SELECT "id" FROM "t" WHERE "n" = '1.5'"#);
    let c0 = &v["queries"][0]["project"]["source"]["filter"]["condition"];
    assert_eq!(c0["operand"][1]["operator"], "01");
    let c1 = &v["queries"][1]["project"]["source"]["filter"]["condition"];
    assert_eq!((c1["operand"][1]["operator"].as_str(), c1["operand"][1]["type"].as_str()), (Some("CAST"), Some("REAL")));
}

// ---------------------------------------------------------------------------------------------------
// #58 (3): citext and char(n)
// ---------------------------------------------------------------------------------------------------

const U: &str = r#"create table "t" ("id" INTEGER, "c" citext, "b" char(3), "v" varchar(3), unique ("id"));
create table "u" ("id" INTEGER, "c" citext, "b" char(5), unique ("id"));"#;

#[test]
fn a_citext_value_is_refused() {
    // `'A' = 'a'` in citext, so on `t = {('A')}` the first returns no rows.
    refused(U, r#"SELECT "c" FROM "t" WHERE "c" = 'A' AND NOT "c" = 'a'"#, r#"SELECT "c" FROM "t" WHERE "c" = 'A'"#, "citext");
    // An opaque type would not do: its `=` is the prover's equality, which substitutes `u.c` for
    // `t.c` under the join, and over `t = {('a')}, u = {('A')}` the two texts differ.
    refused(
        U,
        r#"SELECT "t"."c"::text FROM "t" JOIN "u" ON "t"."c" = "u"."c""#,
        r#"SELECT "u"."c"::text FROM "t" JOIN "u" ON "t"."c" = "u"."c""#,
        "citext",
    );
    // A cast makes one too, in every mode.
    refused(U, r#"SELECT "v"::citext FROM "t""#, r#"SELECT "v" FROM "t""#, "citext");
    refused_in(U, r#"SELECT "v"::citext FROM "t""#, r#"SELECT "v" FROM "t""#, CatalogSource::Inferred, "citext");
}

#[test]
fn a_char_n_value_is_refused() {
    // `char(n)` compares ignoring trailing spaces: in `char(3)`, `'a'` and `'a '` are one value.
    refused(U, r#"SELECT "b" FROM "t" WHERE "b" = 'a' AND NOT "b" = 'a '"#, r#"SELECT "b" FROM "t" WHERE "b" = 'a'"#, "bpchar");
    for target in ["char(3)", "character(3)", "bpchar", "char", "nchar(2)", "char(3)[]"] {
        refused(U, &format!(r#"SELECT "v"::{target} FROM "t""#), r#"SELECT "v" FROM "t""#, "bpchar");
    }
    // A function declared to return one is no different.
    let ddl = format!("declare scalar function f(int) returns char;\n{U}");
    refused(&ddl, r#"SELECT f("id") FROM "t""#, r#"SELECT "id" FROM "t""#, "bpchar");
}

#[test]
fn a_citext_or_char_column_no_query_reads_costs_nothing() {
    for src in [CatalogSource::Declared, CatalogSource::InferredSeeded] {
        let v = lower_in(U, r#"SELECT "id" FROM "t" WHERE "v" = 'x'"#, r#"SELECT "id" FROM "t" WHERE 'x' = "v""#, src);
        assert_eq!(schema_types(&v), ["INTEGER", "VARBINARY", "VARBINARY", "VARCHAR"], "{src:?}");
    }
}

#[test]
fn a_pair_that_lowers_to_one_plan_is_not_refused_for_a_citext_value() {
    // One plan computes one thing, however citext's `=` is read: the two sides differ only in an alias.
    for src in [CatalogSource::Declared, CatalogSource::InferredSeeded] {
        let v = lower_in(U, r#"SELECT "c" FROM "t" WHERE "c" = 'A'"#, r#"SELECT "x"."c" FROM "t" AS "x" WHERE "x"."c" = 'A'"#, src);
        assert_eq!(v["queries"][0], v["queries"][1], "{src:?}");
        assert!(!v.to_string().contains("CITEXT"), "{src:?}: the type leaks to a prover: {v}");
    }
}

#[test]
fn raw_ddl_reads_citext_and_char_the_same_way() {
    let ddl = r#"CREATE TABLE public.t (id integer PRIMARY KEY, c citext, b character(3), v character varying(3));"#;
    let pair = "SELECT \"c\" FROM \"t\" WHERE \"c\" = 'A';\nSELECT \"c\" FROM \"t\";";
    match lower_with_ddl(pair, ddl, CatalogSource::InferredSeeded) {
        Err(FrontendError::Unsupported(m)) => assert!(m.contains("citext"), "{m}"),
        other => panic!("expected a citext refusal, got {other:?}"),
    }
    let ok = lower_with_ddl("SELECT \"id\" FROM \"t\";\nSELECT \"id\" FROM \"t\" WHERE \"v\" = 'x';", ddl, CatalogSource::InferredSeeded)
        .expect("lowers");
    assert_eq!(schema_types(&ok), ["INTEGER", "VARBINARY", "VARBINARY", "VARCHAR"]);
}

// ---------------------------------------------------------------------------------------------------
// #58 (4): floats, and numeric division
// ---------------------------------------------------------------------------------------------------

const F: &str = r#"create table "t" ("x" double precision, "y" double precision, "z" double precision, "r" real, "f" float8, "n" numeric, "a" INTEGER);"#;

#[test]
fn float_addition_is_not_reassociated() {
    // On `t = {(0.1, 0.2, 0.3)}` the first is `0.6000000000000001` and the second `0.6`.
    let v = lower_in(F, r#"SELECT ("x" + "y") + "z" FROM "t""#, r#"SELECT "x" + ("y" + "z") FROM "t""#, CatalogSource::Declared);
    assert_ne!(v["queries"][0], v["queries"][1]);
    assert!(!ops(&v).iter().any(|o| o == "+"), "a native + over floats: {:?}", ops(&v));
    assert_eq!(schema_types(&v), ["VARBINARY", "VARBINARY", "VARBINARY", "VARBINARY", "VARBINARY", "REAL", "INTEGER"]);
    // An integer or numeric operand does not make it exact.
    let v = lower_in(F, r#"SELECT "x" + 1, "r" * "n", "f" - "a" FROM "t""#, r#"SELECT 1 FROM "t""#, CatalogSource::Declared);
    assert!(!ops(&v).iter().any(|o| matches!(o.as_str(), "+" | "*" | "-")), "{:?}", ops(&v));
}

#[test]
fn a_float_cast_is_not_a_real() {
    for src in [CatalogSource::Declared, CatalogSource::InferredSeeded] {
        let v = lower_in(F, r#"SELECT "a"::float8 + "a"::float8 FROM "t""#, r#"SELECT 1 FROM "t""#, src);
        assert_eq!(v["queries"][0]["project"]["target"][0]["type"], "VARBINARY", "{src:?}");
    }
}

#[test]
fn numeric_division_is_not_exact() {
    // `1 / 3.0 * 3.0` is `0.99999999999999999990` in Postgres.
    let v = lower_in(F, r#"SELECT "n" / 3.0 * 3.0 FROM "t""#, r#"SELECT "n" FROM "t""#, CatalogSource::Declared);
    assert!(!ops(&v).iter().any(|o| o == "/"), "a native / over numeric: {:?}", ops(&v));
    assert!(ops(&v).iter().any(|o| o == "q_arith_div_real_real"), "{:?}", ops(&v));
    // Addition, subtraction and multiplication are exact, and stay native.
    let v = lower_in(F, r#"SELECT "n" * 3.0 + 1 - "n" FROM "t""#, r#"SELECT 1 FROM "t""#, CatalogSource::Declared);
    assert!(["*", "+", "-"].iter().all(|o| ops(&v).iter().any(|x| x == o)), "{:?}", ops(&v));
    // Postgres converts the integer side first, so these are one division.
    let v = lower_in(F, r#"SELECT "a" / 2.0 FROM "t""#, r#"SELECT CAST("a" AS numeric) / 2.0 FROM "t""#, CatalogSource::Declared);
    assert_eq!(v["queries"][0], v["queries"][1]);
}

#[test]
fn a_numeric_turned_into_text_is_refused() {
    // A numeric's text shows its scale: for x = 2, `x * 1.0` is '2.0' and `x * 1.00` is '2.00', though
    // both are the REAL 2 to a prover.
    for (q0, q1) in [
        (r#"SELECT CAST("n" * 1.0 AS TEXT) FROM "t""#, r#"SELECT CAST("n" * 1.00 AS TEXT) FROM "t""#),
        (r#"SELECT "n" * 1.0 || 'x' FROM "t""#, r#"SELECT "n" * 1.00 || 'x' FROM "t""#),
        (r#"SELECT ("n" * 1.0)::varchar(8) FROM "t""#, r#"SELECT ("n" * 1.00)::varchar(8) FROM "t""#),
        // Equal numerics, unequal text: over t = {(1.0)}, u = {(1.00)}.
        (
            r#"SELECT "t"."n"::text FROM "t" JOIN "u" ON "t"."n" = "u"."n""#,
            r#"SELECT "u"."n"::text FROM "t" JOIN "u" ON "t"."n" = "u"."n""#,
        ),
        // One spelling, two scales: a symbol keyed on the cast's text would not tell these apart.
        (r#"SELECT CAST("n" AS TEXT) FROM "t""#, r#"SELECT CAST("n" AS TEXT) FROM (SELECT "n" * 1.0 AS "n" FROM "t") AS "s""#),
    ] {
        let ddl = r#"create table "t" ("n" NUMERIC); create table "u" ("n" NUMERIC);"#;
        refused(ddl, q0, q1, "numeric converted to text");
    }
    // An integer's text has no scale to show, and one plan on both sides computes one thing.
    let v = lower(r#"SELECT CAST("a" AS TEXT), "a" || 'x' FROM "t""#, r#"SELECT CAST("a" AS TEXT), "a" || 'x' FROM "t" AS "x""#);
    assert_eq!(v["queries"][0], v["queries"][1]);
    let v = lower(r#"SELECT CAST("n" AS TEXT) FROM "t""#, r#"SELECT CAST("x"."n" AS TEXT) FROM "t" AS "x""#);
    assert_eq!(v["queries"][0], v["queries"][1]);
}

// ---------------------------------------------------------------------------------------------------
// #58 (5): integer types by name
// ---------------------------------------------------------------------------------------------------

#[test]
fn ranges_and_points_are_not_integers() {
    let ddl = r#"create table "t" ("r" int4range, "m" int8range, "p" point, "q" point NOT NULL, "k" int4multirange);"#;
    let v = lower_in(ddl, r#"SELECT ("r" + "r") - "r", ("p" + "q") - "q" FROM "t""#, r#"SELECT "r", "p" FROM "t""#, CatalogSource::Declared);
    assert!(schema_types(&v).iter().all(|t| t != "INTEGER"), "{:?}", schema_types(&v));
    assert!(!ops(&v).iter().any(|o| matches!(o.as_str(), "+" | "-")), "{:?}", ops(&v));
}

/// What one reader makes of a column of type `name` that a query reads: the type the schema gives
/// it, or the refusal.
fn read_column(name: &str, raw_ddl: bool) -> std::result::Result<String, String> {
    let pair = "SELECT \"c\" FROM \"t\";\nSELECT \"c\" FROM \"t\";";
    let r = if raw_ddl {
        lower_with_ddl(pair, &format!("CREATE TABLE t (c {name});"), CatalogSource::Declared)
    } else {
        lower_sql(&format!("create table \"t\" (\"c\" {name});\n{pair}"))
    };
    r.map(|v| schema_types(&v)[0].clone()).map_err(|e| e.to_string())
}

#[test]
fn both_type_readers_agree_on_every_name() {
    // The declared `CREATE TABLE` reader and the raw-DDL reader: a name is refused by both, builtin to
    // both as the same type, or opaque to both (where the declared reader keeps an unknown name as it
    // was spelled, and the raw-DDL reader says VARBINARY).
    let names = [
        "integer", "int", "int2", "int4", "int8", "smallint", "bigint", "serial", "bigserial", "serial4",
        "oid", "numeric", "numeric(10,2)", "decimal", "real", "float", "float4", "float8", "double precision",
        "money", "text", "varchar", "varchar(8)", "character varying(8)", "name", "uuid", "citext",
        "char(3)", "character(3)", "bpchar", "char", "boolean", "bool", "bytea", "int4range", "int8range",
        "point", "jsonb", "inet", "text[]", "integer[]", "citext[]", "date", "timestamp", "timestamptz",
        "interval",
    ];
    let builtin = |t: &str| matches!(t, "INTEGER" | "REAL" | "VARCHAR" | "BOOLEAN" | "DATE" | "TIME" | "TIMESTAMP" | "INTERVAL");
    for name in names {
        match (read_column(name, false), read_column(name, true)) {
            (Err(a), Err(b)) => assert_eq!(a, b, "{name}"),
            (Ok(a), Ok(b)) if builtin(&a) || builtin(&b) => assert_eq!(a, b, "{name}"),
            (Ok(_), Ok(_)) => {}
            (a, b) => panic!("{name}: the declared reader says {a:?}, the raw-DDL reader {b:?}"),
        }
    }
    // And the names this pins: no integer among the ranges and points, no REAL among the floats.
    for name in ["int4range", "point", "real", "double precision", "float8", "money", "uuid"] {
        let t = read_column(name, true).unwrap();
        assert!(!builtin(&t), "{name}: {t}");
    }
    assert_eq!(read_column("numeric", false).unwrap(), "REAL");
}

// ---------------------------------------------------------------------------------------------------
// #67 (2): `parse_declare` and non-ASCII letters
// ---------------------------------------------------------------------------------------------------

#[test]
fn a_declare_line_with_non_ascii_letters_does_not_panic() {
    // `İ` lowercases to three bytes, so an offset found in the lowercased copy fell inside `é`.
    let src = "declare İİ scalar function ée(int) returns int;\n\
               create table t (id INTEGER, a INTEGER, unique (id));\n\
               SELECT id FROM t WHERE g(a) = g(a);\nSELECT id FROM t;";
    let r = std::panic::catch_unwind(|| lower_sql(src));
    assert!(r.is_ok(), "parse_declare panicked");
}

#[test]
fn a_kelvin_sign_does_not_shift_the_declaration() {
    // The Kelvin sign lowercases from three bytes to one, so the return type was read two bytes late,
    // as `URNS TEXT`.
    let src = "declare scalar function g(int /* \u{212A}\u{212A} */) returns text;\n\
               create table t (id INTEGER, a INTEGER, unique (id));\n\
               SELECT id FROM t WHERE g(a) = 'x';\nSELECT id FROM t;";
    let v = lower_sql(src).expect("lowers");
    let call = &v["queries"][0]["project"]["source"]["filter"]["condition"]["operand"][0];
    assert_eq!((call["operator"].as_str(), call["type"].as_str()), (Some("G"), Some("VARCHAR")), "{call}");
    // Before `aggregate`, it moved the name: `g` was left undeclared, and a per-row scalar.
    let src = "declare \u{212A}\u{212A} aggregate function g(int) returns int;\n\
               create table t (id INTEGER, a INTEGER, unique (id));\n\
               SELECT g(a) FROM t;\nSELECT 1 FROM t;";
    let v = lower_sql(src).expect("lowers");
    assert_eq!(v["queries"][0]["group"]["function"][0]["operator"], "G", "{}", v["queries"][0]);
}

// ---------------------------------------------------------------------------------------------------
// #67 (5): the operand of `IN (SELECT ..)` and the subquery's column get one type
// ---------------------------------------------------------------------------------------------------

/// Every `IN` node in `v`, as (operand types, the subquery's column types).
fn in_nodes(v: &Value, out: &mut Vec<(Vec<String>, Vec<String>)>) {
    match v {
        Value::Object(m) => {
            if m.get("operator").and_then(Value::as_str) == Some("IN") && m.contains_key("query") {
                let tys = |a: Option<&Value>| -> Vec<String> {
                    a.and_then(Value::as_array)
                        .map(|a| a.iter().map(|e| e["type"].as_str().unwrap_or("").to_string()).collect())
                        .unwrap_or_default()
                };
                out.push((tys(m.get("operand")), tys(m["query"]["project"].get("target"))));
            }
            m.values().for_each(|x| in_nodes(x, out));
        }
        Value::Array(a) => a.iter().for_each(|x| in_nodes(x, out)),
        _ => {}
    }
}

#[test]
fn an_in_subquery_operand_takes_the_columns_type() {
    // An undeclared function is VARBINARY, `a / 2.0` is REAL: the QED prover asserts that the operand
    // and the column have one sort, and panicked on both.
    for (q0, q1) in [
        (r#"SELECT "id" FROM "t" WHERE "a" IN (SELECT abs("a") FROM "t")"#, r#"SELECT "id" FROM "t" WHERE "a" IN (SELECT abs("a") FROM "t" WHERE "a" IS NOT NULL)"#),
        (r#"SELECT "id" FROM "t" WHERE "id" IN (SELECT "a" / 2.0 FROM "t")"#, r#"SELECT "id" FROM "t" WHERE "id" = ANY (SELECT "a" / 2.0 FROM "t")"#),
        (r#"SELECT "id" FROM "t" WHERE NULL IN (SELECT "a" / 2.0 FROM "t")"#, r#"SELECT "id" FROM "t" WHERE '1' IN (SELECT "a" FROM "t")"#),
        (r#"SELECT "id" FROM "t" WHERE ("a", "id") IN (SELECT "n", abs("a") FROM "t")"#, r#"SELECT "id" FROM "t" WHERE "id" IN (SELECT "n" FROM "t")"#),
    ] {
        for src in [CatalogSource::Declared, CatalogSource::InferredSeeded] {
            let v = lower_in(T, q0, q1, src);
            let mut nodes = Vec::new();
            in_nodes(&v["queries"], &mut nodes);
            assert_eq!(nodes.len(), 2, "{q0}");
            for (operand, column) in nodes {
                assert_eq!(operand, column, "{src:?}: {q0} / {q1}");
            }
        }
    }
}

#[test]
fn an_in_subquery_operand_that_cannot_take_the_columns_type_is_refused() {
    // A numeric against an integer column: Postgres converts the column, which the lowering cannot
    // reach from the operand's side.
    refused(T, r#"SELECT "id" FROM "t" WHERE "n" IN (SELECT "a" FROM "t")"#, r#"SELECT "id" FROM "t""#, "IN subquery");
    refused(T, r#"SELECT "id" FROM "t" WHERE abs("a") IN (SELECT "a" FROM "t")"#, r#"SELECT "id" FROM "t""#, "IN subquery");
}
