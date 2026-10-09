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
//! These pin the lowering and run no prover; the pairs under `tests/pairs/params/` run them.

use sqleq_frontend::{lower_with, CatalogSource, FrontendError};

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
