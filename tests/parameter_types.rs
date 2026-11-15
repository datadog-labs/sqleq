// Unless explicitly stated otherwise all files in this repository are licensed under the
// Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/).
// Copyright 2026-Present Datadog, Inc.

//! An inferred parameter is never narrower than the type Postgres gives it (issue #122).
//!
//! Postgres types an untyped `$N` at its first use, from the operand it meets: in
//! `n + 0 = $1 AND $1 > 0 AND $1 < 1`, over a `numeric` `n`, `$1` is a `numeric`, and on
//! `t = {(1, 0.5)}` with `$1 = '0.5'` the query returns a row. Type inference ranked the literal's
//! integer above an operand it reads no type from, so `$1` was an integer and QED proved the query
//! equivalent to `false`. A lowered query in which an inferred integer parameter meets a `numeric` or
//! float value is now refused (`src/param_types.rs`).
//!
//! The same order decides what a cast over a parameter does. In `n = $1 AND $1::int = 0`, `$1` is a
//! `numeric` and `$1::int` rounds it; the frontend dropped every unqualified cast over a parameter
//! as its type, which made that query one plan with `n = $1 AND $1 = 0`. A cast to a type other than
//! the one inference gave the parameter is now refused (`src/casts.rs`, rule 1).
//!
//! These pin the lowering and run no prover; the pairs under `tests/pairs/params/` run them.

use serde_json::Value;
use sqleq::{lower_with, CatalogSource, FrontendError};

const T: &str = r#"create table "t" ("id" INTEGER, "a" INTEGER, "n" NUMERIC, "f" DOUBLE PRECISION);"#;

fn pair(q0: &str, q1: &str) -> String {
    format!("{T}\n{q0};\n{q1};")
}

fn refused_in(src: CatalogSource, q0: &str, q1: &str) {
    match lower_with(&pair(q0, q1), src) {
        Err(FrontendError::Unsupported(m)) => assert!(
            m.contains("inferred as an integer"),
            "{src:?}: refused for {m:?}\n{q0}\n{q1}"
        ),
        Err(e) => panic!("{src:?}: expected a refusal, got {e}\n{q0}\n{q1}"),
        Ok(_) => panic!("{src:?}: expected a refusal, but it lowered\n{q0}\n{q1}"),
    }
}

fn lowers_in(src: CatalogSource, q0: &str, q1: &str) {
    if let Err(e) = lower_with(&pair(q0, q1), src) {
        panic!("{src:?}: expected Ok, got {e}\n{q0}\n{q1}");
    }
}

/// `q0` and the same query with `$1 < 1` written `$1 <= 0`, a rewrite right for integers only.
fn rewritten(q: &str) -> (String, String) {
    (format!("{q} AND $1 < 1"), format!("{q} AND $1 <= 0"))
}

#[test]
fn an_integer_parameter_next_to_a_numeric_is_refused() {
    let seeded = CatalogSource::InferredSeeded;
    refused_in(
        seeded,
        r#"SELECT "id" FROM "t" WHERE "n" + 0 = $1 AND $1 > 0 AND $1 < 1"#,
        r#"SELECT "id" FROM "t" WHERE false"#,
    );
    for q in [
        r#"SELECT "id" FROM "t" WHERE "n" * 2 = $1"#,
        r#"SELECT "id" FROM "t" GROUP BY "id" HAVING avg("a") = $1"#,
        r#"SELECT "id" FROM "t" WHERE $1 IN (SELECT "n" * 2 FROM "t")"#,
        r#"SELECT "id" FROM "t" WHERE abs("n") = $1"#,
        r#"SELECT "id" FROM "t" WHERE coalesce("n", 0) = $1"#,
        r#"SELECT "id" FROM "t" WHERE (SELECT max("n") FROM "t") = $1"#,
        // Arithmetic types the parameter too, and so does a division by a `numeric` literal.
        r#"SELECT "id" FROM "t" WHERE "n" + $1 > 0"#,
        r#"SELECT "id" FROM "t" WHERE $1 / 2.0 > "n""#,
        r#"SELECT "id" FROM "t" WHERE greatest("n", $1) > 0"#,
        // A float: `double precision` arithmetic.
        r#"SELECT "id" FROM "t" WHERE "f" * 2 = $1"#,
    ] {
        let (q0, q1) = rewritten(q);
        refused_in(seeded, &q0, &q1);
    }
    refused_in(
        seeded,
        r#"SELECT "id" FROM "t" WHERE "n" - 0 = $1 AND $1 BETWEEN 0 AND 1"#,
        r#"SELECT "id" FROM "t" WHERE "n" - 0 = $1 AND ($1 = 0 OR $1 = 1)"#,
    );
    refused_in(
        seeded,
        r#"SELECT "n" + 0 = $1, $1 < 1 FROM "t""#,
        r#"SELECT "n" + 0 = $1, $1 <= 0 FROM "t""#,
    );
    // Two queries that lower to one plan are not exempt: the order Postgres types `$1` in is the
    // statement's, `numeric` here and integer if `$1 < 1` came first.
    let q = r#"SELECT "id" FROM "t" WHERE "n" * 2 = $1 AND $1 < 1"#;
    refused_in(seeded, q, r#"SELECT "id" FROM "t" WHERE $1 < 1 AND "n" * 2 = $1"#);
    // Under the inferring catalog, which types the columns from their use too.
    let (q0, q1) = rewritten(r#"SELECT "id" FROM "t" GROUP BY "id" HAVING avg("a") = $1"#);
    refused_in(CatalogSource::Inferred, &q0, &q1);
}

#[test]
fn a_parameter_postgres_types_as_an_integer_still_lowers() {
    for src in [CatalogSource::InferredSeeded, CatalogSource::Inferred] {
        for q in [
            // `$1 + 0` types `$1` before the comparison does: an integer.
            r#"SELECT "id" FROM "t" WHERE "n" = $1 + 0"#,
            // An integer column.
            r#"SELECT "id" FROM "t" WHERE "a" = $1"#,
        ] {
            let (q0, q1) = rewritten(q);
            lowers_in(src, &q0, &q1);
        }
    }
    let seeded = CatalogSource::InferredSeeded;
    // A bare `numeric` column: inference unifies `$1` with it, a `numeric` too.
    let (q0, q1) = rewritten(r#"SELECT "id" FROM "t" WHERE "n" = $1"#);
    lowers_in(seeded, &q0, &q1);
    // An argument whose type the function fixes, and a count.
    lowers_in(
        seeded,
        r#"SELECT "id" FROM "t" WHERE round("n", $1) > 0"#,
        r#"SELECT "id" FROM "t" WHERE round("n", $1) > 1"#,
    );
    lowers_in(
        seeded,
        r#"SELECT "id" FROM "t" WHERE "a" = $1 LIMIT $2"#,
        r#"SELECT "id" FROM "t" WHERE "a" = $1 ORDER BY "id" LIMIT $2"#,
    );
    // A parameter cast wherever it appears is typed by its cast, whichever use comes first.
    let (q0, q1) = (
        r#"SELECT "id" FROM "t" WHERE "n" * 2 = $1::int AND $1::int < 1"#,
        r#"SELECT "id" FROM "t" WHERE "n" * 2 = $1::int AND $1::int <= 0"#,
    );
    lowers_in(seeded, q0, q1);
    // One bare use is enough for the order to matter again.
    refused_in(
        seeded,
        r#"SELECT "id" FROM "t" WHERE "n" * 2 = $1 AND $1::int < 1"#,
        r#"SELECT "id" FROM "t" WHERE "n" * 2 = $1 AND $1::int <= 0"#,
    );
}

#[test]
fn a_declared_parameter_type_is_the_client_s() {
    // A client that declares the parameter an integer gets an integer comparison, as Postgres gives it.
    let sql = format!(
        "{T}\ndeclare function QP1(INTEGER) returns INTEGER;\n\
         SELECT \"id\" FROM \"t\" WHERE \"n\" * 2 = $1 AND $1 < 1;\n\
         SELECT \"id\" FROM \"t\" WHERE \"n\" * 2 = $1 AND $1 <= 0;"
    );
    if let Err(e) = lower_with(&sql, CatalogSource::InferredSeeded) {
        panic!("expected Ok, got {e}");
    }
}

/// A cast to the type its operand already has is the identity, so `$2::uuid::uuid` and `$2::uuid`
/// are one plan. The inner cast is the parameter's own type and dropped; the outer one used to be
/// read through `q_exact_varbinary`, which split the plans.
#[test]
fn a_repeated_cast_over_a_parameter_lowers_to_one_plan() {
    let ddl = r#"create table "t" ("id" UUID PRIMARY KEY, "a" INTEGER);"#;
    let one = r#"UPDATE "t" SET "a" = $1 WHERE "id" = $2::uuid"#;
    for other in [
        r#"UPDATE "t" SET "a" = $1 WHERE "id" = $2::uuid::UUID"#,
        r#"UPDATE "t" SET "a" = $1 WHERE "id" = ($2::uuid)::uuid"#,
        r#"UPDATE "t" SET "a" = $1 WHERE "id" = CAST($2::uuid AS uuid)"#,
    ] {
        let v: Value = lower_with(&format!("{ddl}\n{other};\n{one};"), CatalogSource::InferredSeeded)
            .unwrap_or_else(|e| panic!("expected Ok, got {e}\n{other}"));
        assert_eq!(v["queries"][0], v["queries"][1], "{other}");
    }
}

#[test]
fn a_cast_over_a_parameter_to_another_type_is_refused() {
    let q0 = r#"SELECT "id" FROM "t" WHERE "n" = $1 AND $1::int = 0"#;
    let q1 = r#"SELECT "id" FROM "t" WHERE "n" = $1 AND $1 = 0"#;
    // Under the seeded catalog: the inferring one types `n` from its use, an integer here, which
    // makes the cast the parameter's own type there.
    match lower_with(&pair(q0, q1), CatalogSource::InferredSeeded) {
        Err(FrontendError::Unsupported(m)) => {
            assert!(m.contains("is cast to INT but inferred as numeric"), "refused for {m:?}")
        }
        other => panic!("expected a refusal, got {other:?}"),
    }
    // A cast to the parameter's own type is its type and nothing more, and still dropped.
    for (q0, q1) in [
        (r#"SELECT "id" FROM "t" WHERE "a" = $1::bigint"#, r#"SELECT "id" FROM "t" WHERE "a" = $1"#),
        (
            r#"SELECT "id" FROM "t" WHERE "n" > $1::numeric"#,
            r#"SELECT "id" FROM "t" WHERE "n" >= $1::numeric"#,
        ),
        (
            r#"SELECT "id" FROM "t" WHERE "a" = $1 LIMIT $2::int"#,
            r#"SELECT "id" FROM "t" WHERE "a" = $1 LIMIT $2"#,
        ),
        // An array cast states its element's type, the one inference gives an array parameter.
        (
            r#"SELECT "id" FROM "t" WHERE "a" = ANY($1::bigint[])"#,
            r#"SELECT "id" FROM "t" WHERE "a" = ANY($1::integer[])"#,
        ),
    ] {
        lowers_in(CatalogSource::InferredSeeded, q0, q1);
    }
}

/// The type the lowered pair gives `QP1`.
fn qp1_type(q: &str) -> String {
    fn find(v: &Value) -> Option<String> {
        match v {
            Value::Object(m) if m.get("operator").and_then(Value::as_str) == Some("QP1") => {
                m.get("type").and_then(Value::as_str).map(str::to_string)
            }
            Value::Object(m) => m.values().find_map(find),
            Value::Array(a) => a.iter().find_map(find),
            _ => None,
        }
    }
    let v = lower_with(&pair(q, q), CatalogSource::InferredSeeded)
        .unwrap_or_else(|e| panic!("expected Ok, got {e}\n{q}"));
    find(&v["queries"][0]).unwrap_or_else(|| panic!("no QP1 in {q}"))
}

#[test]
fn a_parameter_in_arithmetic_takes_its_partners_type() {
    // Postgres gives `$1` the type of the operand it meets: a `numeric` column's, a literal's.
    assert_eq!(qp1_type(r#"SELECT "id" FROM "t" WHERE "n" + $1 > 0"#), "REAL");
    assert_eq!(qp1_type(r#"SELECT "id" FROM "t" WHERE "a" + $1 > 0"#), "INTEGER");
    assert_eq!(qp1_type(r#"SELECT "id" FROM "t" WHERE $1 + 1 > "a""#), "INTEGER");
    // Next to an operand inference cannot type, no guess: an integer here would be narrower than
    // the `numeric` Postgres gives it.
    assert_eq!(qp1_type(r#"SELECT sum("n") / $1 FROM "t""#), "VARBINARY");
}

/// The type the plan gives `$k`, by its `QPk` node.
fn param_type(q: &str, k: u32) -> String {
    fn find(v: &Value, name: &str) -> Option<String> {
        match v {
            Value::Object(m) if m.get("operator").and_then(Value::as_str) == Some(name) => {
                m.get("type").and_then(Value::as_str).map(str::to_string)
            }
            Value::Object(m) => m.values().find_map(|x| find(x, name)),
            Value::Array(a) => a.iter().find_map(|x| find(x, name)),
            _ => None,
        }
    }
    let ddl = r#"create table "t" ("id" INTEGER, "a" INTEGER, "n" NUMERIC, "s" TEXT, "p" BOOLEAN);"#;
    let v = lower_with(&format!("{ddl}\n{q};\n{q};"), CatalogSource::InferredSeeded)
        .unwrap_or_else(|e| panic!("expected Ok, got {e}\n{q}"));
    find(&v["queries"][0], &format!("QP{k}")).unwrap_or_else(|| panic!("no QP{k} in {q}"))
}

#[test]
fn a_parameter_among_coalesce_arguments_takes_the_others_type() {
    // Postgres resolves the arguments of COALESCE, NULLIF, GREATEST and LEAST to one type, so a
    // parameter there takes the others' (#174). It was opaque, and `NOT` over the opaque COALESCE
    // reached the QED prover ill-sorted.
    assert_eq!(param_type(r#"SELECT NOT COALESCE("s" = $1, $2) AS "x" FROM "t""#, 2), "BOOLEAN");
    assert_eq!(param_type(r#"SELECT COALESCE("p", $1) FROM "t""#, 1), "BOOLEAN");
    assert_eq!(param_type(r#"SELECT COALESCE("a", $1) FROM "t""#, 1), "INTEGER");
    assert_eq!(param_type(r#"SELECT COALESCE("s", $1) FROM "t""#, 1), "VARCHAR");
    assert_eq!(param_type(r#"SELECT NULLIF("a", $1) FROM "t""#, 1), "INTEGER");
    assert_eq!(param_type(r#"SELECT GREATEST("a", 1.5, $1) FROM "t""#, 1), "REAL");
    // Evidence that does not agree says nothing.
    assert_eq!(param_type(r#"SELECT COALESCE("s", "a", $1) FROM "t""#, 1), "VARBINARY");
    // A parameter something else typed keeps that type.
    assert_eq!(param_type(r#"SELECT COALESCE("a", $1) FROM "t" WHERE $1 = "n""#, 1), "REAL");
    // Two columns of two numeric types are not unified: `COALESCE(a, n)` still lowers.
    lowers_in(CatalogSource::InferredSeeded, r#"SELECT COALESCE("a", "n") FROM "t""#, r#"SELECT COALESCE("a", "n") FROM "t""#);
}

/// The refusal's message for `q` against itself under the seeded catalog and `ddl`.
fn nullif_refusal(ddl: &str, q: &str) -> String {
    match lower_with(&format!("{ddl}\n{q};\n{q};"), CatalogSource::InferredSeeded) {
        Err(FrontendError::Unsupported(m)) => m,
        Err(e) => panic!("expected a refusal, got {e}\n{q}"),
        Ok(v) => panic!("expected a refusal, but it lowered to {v}\n{q}"),
    }
}

#[test]
fn an_untyped_parameter_as_nullifs_second_argument_takes_the_firsts_type() {
    // Inference reads no type off an aggregate or an expression, so `$1` beside `SUM(a)` reaches the
    // lowering untyped. Postgres types it as NULLIF's first argument (`PREPARE` says `bigint` for the
    // first of these), and so does the lowering; it was refused as a VARBINARY against a REAL (#251).
    assert_eq!(param_type(r#"SELECT "id", NULLIF(SUM("a"), $1) FROM "t" GROUP BY "id""#, 1), "REAL");
    assert_eq!(param_type(r#"SELECT "id", NULLIF(COUNT(*), $1) FROM "t" GROUP BY "id""#, 1), "INTEGER");
    assert_eq!(param_type(r#"SELECT NULLIF("a" + 1, $1) FROM "t""#, 1), "INTEGER");
    assert_eq!(param_type(r#"SELECT NULLIF(SUM("n"), $1) FROM "t""#, 1), "REAL");
    // It is the parameter that takes the type: there is no cast of it.
    let ddl = r#"create table "t" ("id" INTEGER, "a" INTEGER, "n" NUMERIC, "s" TEXT, "p" BOOLEAN);"#;
    let q = r#"SELECT "id", NULLIF(SUM("a"), $1) FROM "t" GROUP BY "id""#;
    let v = lower_with(&format!("{ddl}\n{q};\n{q};"), CatalogSource::InferredSeeded).unwrap();
    assert!(!v.to_string().contains(r#""operator":"CAST""#), "{v}");

    // A parameter with a type of its own is a typed value, and is checked as one: over an INTEGER,
    // `$1::numeric` compares at numeric, and cast to INTEGER 1.5 would be 2.
    let m = nullif_refusal(ddl, r#"SELECT NULLIF("a", $1::numeric) FROM "t""#);
    assert!(m.contains("NULLIF of a value of type INTEGER against one of type REAL"), "{m}");
    // So is one a client declared, of an opaque type included.
    let declared = format!("{ddl}\ndeclare function QP1(INTEGER) returns DOUBLE PRECISION;");
    let m = nullif_refusal(&declared, r#"SELECT "id", NULLIF(SUM("a"), $1) FROM "t" GROUP BY "id""#);
    assert!(m.contains("NULLIF of a value of type REAL against one of type VARBINARY"), "{m}");
}

#[test]
fn inputs_that_are_all_of_unknown_type_are_text() {
    // Postgres resolves the results of a CASE, and the arguments of COALESCE and its kin, to one
    // type, and to `text` when every one is of unknown type: `CASE WHEN p THEN $1 ELSE $2 END`
    // prepares with both parameters `text`. They were left opaque, and a set operation pairing such
    // a CASE with a text column was refused (#214).
    for k in [1, 2] {
        assert_eq!(param_type(r#"SELECT CASE WHEN "p" THEN $1 ELSE $2 END FROM "t""#, k), "VARCHAR");
        assert_eq!(param_type(r#"SELECT COALESCE($1, $2) FROM "t""#, k), "VARCHAR");
        assert_eq!(param_type(r#"SELECT GREATEST($1, $2) FROM "t""#, k), "VARCHAR");
        assert_eq!(param_type(r#"SELECT NULLIF($1, $2) FROM "t""#, k), "VARCHAR");
    }
    // A NULL is of unknown type too, written or implied by a missing ELSE.
    assert_eq!(param_type(r#"SELECT CASE WHEN "p" THEN $1 ELSE NULL END FROM "t""#, 1), "VARCHAR");
    assert_eq!(param_type(r#"SELECT CASE WHEN "p" THEN $1 END FROM "t""#, 1), "VARCHAR");
    assert_eq!(param_type(r#"SELECT COALESCE($1, NULL) FROM "t""#, 1), "VARCHAR");

    // One input with a type decides, as before.
    assert_eq!(param_type(r#"SELECT CASE WHEN "p" THEN $1 ELSE 0 END FROM "t""#, 1), "INTEGER");
    assert_eq!(param_type(r#"SELECT CASE WHEN "p" THEN $1 ELSE "n" END FROM "t""#, 1), "REAL");
    // So does another use of the parameter, anywhere in the pair: the default is taken last.
    for k in [1, 2] {
        assert_eq!(param_type(r#"SELECT CASE WHEN "p" THEN $1 ELSE $2 END FROM "t" WHERE "a" = $2"#, k), "INTEGER");
    }
    assert_eq!(param_type(r#"SELECT COALESCE($1, $2) FROM "t" WHERE "a" = $2"#, 2), "INTEGER");
    // A parameter under a cast is not of unknown type, and types the rest.
    assert_eq!(param_type(r#"SELECT CASE WHEN "p" THEN $1 ELSE $2::integer END FROM "t""#, 1), "INTEGER");

    // Two parameters that are only compared are of unknown type to this pass, and stay so: nothing
    // makes `$1 = $2` a comparison of strings. (Opaque is the wider reading.)
    assert_eq!(param_type(r#"SELECT "id" FROM "t" WHERE $1 = $2"#, 1), "VARBINARY");
    // The condition's parameters are not the results'.
    assert_eq!(param_type(r#"SELECT CASE WHEN $1 = $2 THEN $3 ELSE $4 END FROM "t""#, 1), "VARBINARY");
    assert_eq!(param_type(r#"SELECT CASE WHEN $1 = $2 THEN $3 ELSE $4 END FROM "t""#, 3), "VARCHAR");
}

#[test]
fn a_column_nobody_declared_is_not_typed_text_by_a_case_beside_it() {
    // In the catalog that infers its columns, `x` has no type, and it is in `$1`'s class. Resolving
    // that class as text would type a column of a table nobody declared, so it is left alone.
    let q = r#"SELECT CASE WHEN true THEN $1 ELSE $2 END FROM "u" WHERE "x" = $1"#;
    let v = lower_with(&format!("{q};\n{q};"), CatalogSource::Inferred).unwrap_or_else(|e| panic!("expected Ok, got {e}"));
    let text = v.to_string();
    assert!(!text.contains(r#""operator":"QP1","type":"VARCHAR""#) && !text.contains(r#""type":"VARCHAR","operator":"QP1""#), "{v}");
    assert!(!v["schemas"].to_string().contains("VARCHAR"), "{v}");
}

/// A table with columns of types inference has no class for, for the tests of #285.
const U: &str = r#"create table "t" ("id" INTEGER PRIMARY KEY, "a" BIGINT, "u" UUID, "f" DOUBLE PRECISION, "j" JSONB, "tags" TEXT[], "ts" TIMESTAMP, "s" TEXT);"#;

fn lowered(src: CatalogSource, q0: &str, q1: &str) -> Result<Value, FrontendError> {
    lower_with(&format!("{U}\n{q0};\n{q1};"), src)
}

/// A cast over a parameter is the identity only where Postgres gave the parameter that type. For a
/// type inference has no class for, the cast is dropped where every use of the parameter states the
/// type: a cast to it, a comparison with a column the DDL declares of it, a value stored in one.
#[test]
fn a_cast_to_the_type_every_use_states_is_one_plan_with_the_bare_parameter() {
    for (q0, q1) in [
        (r#"SELECT "id" FROM "t" WHERE "u" = $1::uuid"#, r#"SELECT "id" FROM "t" WHERE "u" = $1"#),
        (r#"SELECT "id" FROM "t" WHERE "u" IN ($1::uuid, $2::uuid)"#, r#"SELECT "id" FROM "t" WHERE "u" IN ($1, $2)"#),
        (r#"SELECT "id" FROM "t" WHERE "a" = ANY($1::bigint[])"#, r#"SELECT "id" FROM "t" WHERE "a" = ANY($1)"#),
        (r#"SELECT "id" FROM "t" WHERE "u" = ANY($1::uuid[])"#, r#"SELECT "id" FROM "t" WHERE "u" = ANY($1)"#),
        (r#"SELECT "id" FROM "t" WHERE "tags" = $1::text[]"#, r#"SELECT "id" FROM "t" WHERE "tags" = $1"#),
        (r#"SELECT "id" FROM "t" WHERE "f" = $1::float8"#, r#"SELECT "id" FROM "t" WHERE "f" = $1"#),
        (r#"SELECT "id" FROM "t" WHERE "f" BETWEEN $1::double precision AND $2"#, r#"SELECT "id" FROM "t" WHERE "f" BETWEEN $1 AND $2"#),
        (r#"UPDATE "t" SET "j" = $1::jsonb WHERE "id" = $2"#, r#"UPDATE "t" SET "j" = $1 WHERE "id" = $2"#),
        (r#"INSERT INTO "t" ("id", "u") VALUES ($1, $2::uuid)"#, r#"INSERT INTO "t" ("id", "u") VALUES ($1, $2)"#),
    ] {
        let v = lowered(CatalogSource::InferredSeeded, q0, q1).unwrap_or_else(|e| panic!("expected Ok, got {e}\n{q0}\n{q1}"));
        assert_eq!(v["queries"][0], v["queries"][1], "{q0}\n{q1}");
    }
}

#[test]
fn a_cast_to_a_type_some_use_does_not_state_is_refused() {
    for (q0, q1) in [
        // #285's three pairs: an array is not its element.
        (r#"SELECT "id" FROM "t" WHERE "a" = ($1)::bigint[]"#, r#"SELECT "id" FROM "t" WHERE "a" = ($1)::bigint"#),
        (r#"SELECT "id", ($1)::bigint[] AS "x" FROM "t""#, r#"SELECT "id", ($1)::bigint AS "x" FROM "t""#),
        (r#"SELECT ($1)::bigint[] AS "s" UNION ALL SELECT "s" FROM "t""#, r#"SELECT $1 AS "s" UNION ALL SELECT "s" FROM "t""#),
        // The column is another type than the cast's: `float4` rounds what a `double precision`
        // parameter holds, and a `date` drops what a `timestamp` one holds.
        (r#"SELECT "id" FROM "t" WHERE "f" = $1::float4"#, r#"SELECT "id" FROM "t" WHERE "f" = $1"#),
        (r#"SELECT "id" FROM "t" WHERE "ts" = ANY($1::date[])"#, r#"SELECT "id" FROM "t" WHERE "ts" = ANY($1)"#),
        (r#"SELECT "id" FROM "t" WHERE "j" = $1::json"#, r#"SELECT "id" FROM "t" WHERE "j" = $1::jsonb"#),
        // A use that states no type.
        (r#"SELECT "id", ($1)::jsonb AS "x" FROM "t""#, r#"SELECT "id", $1 AS "x" FROM "t""#),
        (r#"SELECT "id" FROM "t" WHERE "u" = $1::uuid"#, r#"SELECT "id" FROM "t" WHERE "u" = $1 OR lower($1) = 'x'"#),
        // One query, two casts, in two orders.
        (
            r#"SELECT "id" FROM "t" WHERE "a" = ANY($1::int[]) AND "s" = ANY($1::text[])"#,
            r#"SELECT "id" FROM "t" WHERE "s" = ANY($1::text[]) AND "a" = ANY($1::int[])"#,
        ),
    ] {
        match lowered(CatalogSource::InferredSeeded, q0, q1) {
            Err(FrontendError::Unsupported(m)) => {
                assert!(m.contains("not every use of $1 states that type"), "refused for {m:?}\n{q0}\n{q1}")
            }
            other => panic!("expected a refusal, got {other:?}\n{q0}\n{q1}"),
        }
    }
}

/// A pair that numbers its parameters differently is refused for that, as before: there two uses of
/// `$1` are two parameters, and that they state two types is the misalignment's doing.
#[test]
fn a_misaligned_pair_is_still_refused_as_misaligned() {
    for (q0, q1) in [
        (
            r#"SELECT "id" FROM "t" WHERE "u" = $1::uuid AND "j" = $2::jsonb"#,
            r#"SELECT "id" FROM "t" WHERE "j" = $1::jsonb AND "u" = $2::uuid"#,
        ),
        (
            r#"SELECT "id" FROM "t" WHERE "u" = $1::uuid AND "f" = $2"#,
            r#"SELECT "id" FROM "t" WHERE "f" = $1 AND "u" = $2::uuid"#,
        ),
    ] {
        match lowered(CatalogSource::InferredSeeded, q0, q1) {
            Err(FrontendError::ParameterMisaligned(_)) => {}
            other => panic!("expected a misalignment, got {other:?}\n{q0}\n{q1}"),
        }
    }
}

/// Temporal columns, for the tests of #299.
const TEMPORAL: &str = r#"create table "t" ("id" INTEGER PRIMARY KEY, "ts" TIMESTAMP, "tz" TIMESTAMPTZ, "d" DATE, "s" TEXT);"#;

/// A cast over a parameter is a conversion wherever some other use makes the parameter another
/// type, a type inference has a class for included: it keeps the temporal classes apart in a
/// comparison, so its own test did not see these.
#[test]
fn a_temporal_cast_beside_a_column_of_another_temporal_type_is_refused() {
    for (q0, q1) in [
        // A `date` drops what a `timestamp` parameter holds.
        (r#"SELECT "id" FROM "t" WHERE "ts" = $1::date"#, r#"SELECT "id" FROM "t" WHERE "ts" = $1"#),
        // A `timestamptz` reads an offset that a `timestamp` drops.
        (r#"SELECT "id" FROM "t" WHERE "ts" = $1::timestamptz"#, r#"SELECT "id" FROM "t" WHERE "ts" = $1"#),
        (r#"SELECT "id" FROM "t" WHERE "d" = $1::timestamp"#, r#"SELECT "id" FROM "t" WHERE "d" = $1"#),
        (r#"SELECT "id" FROM "t" WHERE "ts" BETWEEN $1::date AND $2"#, r#"SELECT "id" FROM "t" WHERE "ts" BETWEEN $1 AND $2"#),
        // A bare select item is a `text`.
        (r#"SELECT "id", $1::timestamptz AS "at" FROM "t""#, r#"SELECT "id", $1 AS "at" FROM "t""#),
        (r#"SELECT "id", ($1)::bigint AS "x" FROM "t""#, r#"SELECT "id", $1 AS "x" FROM "t""#),
    ] {
        match lower_with(&format!("{TEMPORAL}\n{q0};\n{q1};"), CatalogSource::InferredSeeded) {
            Err(FrontendError::Unsupported(m)) => {
                assert!(m.contains("not every use of $1 states that type"), "refused for {m:?}\n{q0}\n{q1}")
            }
            other => panic!("expected a refusal, got {other:?}\n{q0}\n{q1}"),
        }
    }
}

#[test]
fn a_temporal_cast_to_the_type_its_other_uses_state_is_still_one_plan() {
    for (q0, q1) in [
        (r#"SELECT "id" FROM "t" WHERE "ts" = $1::timestamp"#, r#"SELECT "id" FROM "t" WHERE "ts" = $1"#),
        (r#"SELECT "id" FROM "t" WHERE "tz" = $1::timestamptz"#, r#"SELECT "id" FROM "t" WHERE "tz" = $1"#),
        (r#"SELECT "id" FROM "t" WHERE "d" = $1::date"#, r#"SELECT "id" FROM "t" WHERE "d" = $1"#),
        // Compared with a cast, the parameter has the cast's type.
        (r#"SELECT "id" FROM "t" WHERE "ts"::date = $1::date"#, r#"SELECT "id" FROM "t" WHERE "ts"::date = $1"#),
        (r#"SELECT "id", $1::text AS "x" FROM "t""#, r#"SELECT "id", $1 AS "x" FROM "t""#),
        // A branch of a set operation is typed by the other branch, not as a select item.
        (r#"SELECT $1::text AS "s" UNION ALL SELECT "s" FROM "t""#, r#"SELECT $1 AS "s" UNION ALL SELECT "s" FROM "t""#),
        (r#"UPDATE "t" SET "ts" = $1::timestamp WHERE "id" = $2"#, r#"UPDATE "t" SET "ts" = $1 WHERE "id" = $2"#),
    ] {
        let v = lower_with(&format!("{TEMPORAL}\n{q0};\n{q1};"), CatalogSource::InferredSeeded)
            .unwrap_or_else(|e| panic!("expected Ok, got {e}\n{q0}\n{q1}"));
        assert_eq!(v["queries"][0], v["queries"][1], "{q0}\n{q1}");
    }
}
