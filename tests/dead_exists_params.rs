// Unless explicitly stated otherwise all files in this repository are licensed under the
// Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/).
// Copyright 2026-Present Datadog, Inc.

//! A parameter that is only an `EXISTS` subquery's select-list item does not count toward arity.
//!
//! `EXISTS (SELECT $1 FROM s ..)` against `EXISTS (SELECT 1 FROM s ..)` made the pair's parameter
//! sets differ, so it was refused as `parameter-misaligned` although `$1` cannot change the result:
//! `EXISTS` asks only whether a row comes back. `params::mentioned` now leaves such a parameter out,
//! except where the other query mentions it, and keeps it where it is not dead: cast, used elsewhere,
//! or under a set operation that compares the values.
//!
//! These pin the lowering and run no prover; the pairs under `tests/pairs/params/` run them.

use sqleq_frontend::{lower_with, CatalogSource, FrontendError};

const T: &str = r#"create table "t" ("id" INTEGER PRIMARY KEY, "a" INTEGER);
create table "s" ("id" INTEGER PRIMARY KEY, "t_id" INTEGER);"#;

const B: &str = r#"SELECT "id" FROM "t" WHERE EXISTS (SELECT 1 FROM "s" WHERE "s"."t_id" = "t"."id") AND "a" = $2"#;

fn lowered(q0: &str, q1: &str) -> Result<(), FrontendError> {
    lower_with(&format!("{T}\n{q0};\n{q1};"), CatalogSource::InferredSeeded).map(|_| ())
}

fn misaligned(q0: &str, q1: &str) {
    match lowered(q0, q1) {
        Err(FrontendError::ParameterMisaligned(m)) => assert!(m.starts_with("arity"), "{m}"),
        other => panic!("expected an arity misalignment, got {other:?}\n{q0}\n{q1}"),
    }
}

#[test]
fn a_dead_exists_parameter_is_left_out_of_arity() {
    for a in [
        r#"SELECT "id" FROM "t" WHERE EXISTS (SELECT $1 FROM "s" WHERE "s"."t_id" = "t"."id") AND "a" = $2"#,
        r#"SELECT "id" FROM "t" WHERE NOT NOT EXISTS (SELECT ($1) AS x FROM "s" WHERE "s"."t_id" = "t"."id") AND "a" = $2"#,
    ] {
        if let Err(e) = lowered(a, B) {
            panic!("expected it to lower, got {e}\n{a}");
        }
    }
}

#[test]
fn a_parameter_that_is_not_dead_still_counts() {
    // Cast: binding a value the cast cannot take is an error.
    misaligned(
        r#"SELECT "id" FROM "t" WHERE EXISTS (SELECT $1::int FROM "s" WHERE "s"."t_id" = "t"."id") AND "a" = $2"#,
        B,
    );
    // Also used in the subquery's condition.
    misaligned(
        r#"SELECT "id" FROM "t" WHERE EXISTS (SELECT $1 FROM "s" WHERE "s"."t_id" = $1) AND "a" = $2"#,
        B,
    );
    // Under a set operation, where the values decide which rows `EXCEPT` keeps.
    misaligned(
        r#"SELECT "id" FROM "t" WHERE EXISTS (SELECT $1 FROM "s" EXCEPT SELECT "t_id" FROM "s") AND "a" = $2"#,
        B,
    );
}
