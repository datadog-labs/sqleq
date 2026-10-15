// Unless explicitly stated otherwise all files in this repository are licensed under the
// Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/).
// Copyright 2026-Present Datadog, Inc.

//! A call is never lowered as an operator the provers already interpret (issue #163).
//!
//! A call is lowered as an operator named after it. `"<"("a", 1)` calls a function the DDL may
//! declare, but lowered by its name it was the comparison `a < 1`, so the pair was one plan and
//! proved. The same holds for any name the provers or the frontend give a meaning to: the
//! connectives and syntax forms (`AND`, `CASE`), the word forms of arithmetic (`DIV` is `/` to the
//! provers, while Postgres's `div` truncates), the frontend's parameter symbols (`QP1`) and its
//! synthetic functions (`q_str_lt`). Such a call is now refused (`src/lower.rs`,
//! `refuse_reserved_call_names`); the Postgres functions whose meaning the provers share still lower.
//!
//! These pin the lowering and run no prover; `tests/pairs/calls/function_named_like_an_operator.sql`
//! runs the issue's pair.

use sqleq_frontend::{lower_with, CatalogSource, FrontendError};

const T: &str = r#"create table "t" ("id" INTEGER, "a" INTEGER, "n" NUMERIC, "s" TEXT);"#;

fn lowered(q0: &str, q1: &str) -> Result<(), FrontendError> {
    lower_with(&format!("{T}\n{q0};\n{q1};"), CatalogSource::Declared).map(|_| ())
}

#[test]
fn a_call_named_like_an_operator_is_refused() {
    for call in [
        r#""<"("a", 1)"#,
        r#""="("a", 1)"#,
        r#""AND"("a" > 0, "a" < 9)"#,
        r#""case"("a" > 0)"#,
        r#"eq("a", 1)"#,
        r#"div("n", 2) > 0"#,
        r#"qp1(0) > 0"#,
        r#"q_str_lt("s", 'a')"#,
        r#""any"("a") > 0"#,
        r#"public."<"("a", 1)"#,
    ] {
        let q = format!(r#"SELECT "id" FROM "t" WHERE {call}"#);
        match lowered(&q, r#"SELECT "id" FROM "t""#) {
            Err(FrontendError::Unsupported(m)) => {
                assert!(m.contains("which the IR reads as its own operator"), "{call}: refused for {m:?}")
            }
            Err(e) => panic!("{call}: expected the reserved-name refusal, got {e}"),
            Ok(()) => panic!("{call}: expected a refusal, but it lowered"),
        }
    }
}

#[test]
fn postgres_functions_the_provers_share_still_lower() {
    for call in [r#"power("a", 2) > 0"#, r#"coalesce("a", 0) > 0"#, r#"upper("s") = 'A'"#, r#"quote_ident("s") = 'a'"#] {
        let q = format!(r#"SELECT "id" FROM "t" WHERE {call}"#);
        if let Err(e) = lowered(&q, r#"SELECT "id" FROM "t""#) {
            panic!("{call}: expected it to lower, got {e}");
        }
    }
}
