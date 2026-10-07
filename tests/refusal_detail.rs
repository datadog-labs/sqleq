// Unless explicitly stated otherwise all files in this repository are licensed under the
// Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/).
// Copyright 2026-Present Datadog, Inc.

//! Two refusals and what they say. A cast Postgres does not have (`TRY_CAST`, `SAFE_CAST`) is
//! refused rather than lowered as the `CAST` sqlparser builds it as, and a bare column that more than
//! one table in scope could own is refused naming the column and those tables.

use sqleq_frontend::{lower_with, CatalogSource, FrontendError};

const T: &str = r#"create table "t" ("id" INTEGER, "b" VARCHAR, unique ("id"));"#;

/// Every catalog source: the refusal is about syntax, so none of them may let it through.
const SOURCES: [CatalogSource; 3] =
    [CatalogSource::Declared, CatalogSource::InferredSeeded, CatalogSource::Inferred];

fn unsupported(src: &str, source: CatalogSource) -> String {
    match lower_with(src, source) {
        Err(FrontendError::Unsupported(m)) => m,
        other => panic!("{source:?}: expected an unsupported refusal, got {other:?}\n{src}"),
    }
}

#[test]
fn try_cast_and_safe_cast_are_refused_rather_than_lowered_as_cast() {
    // `TRY_CAST('x' AS integer)` is NULL where `CAST` raises, so the two sides are not one query;
    // read as a plain `CAST`, both used to lower to the same plan.
    for kw in ["TRY_CAST", "SAFE_CAST"] {
        for source in SOURCES {
            let one_side = format!(
                "{T}\nSELECT \"id\", {kw}(\"b\" AS integer) FROM \"t\";\nSELECT \"id\", CAST(\"b\" AS integer) FROM \"t\";"
            );
            assert_eq!(unsupported(&one_side, source), kw);
            // Both sides alike is refused too: what is refused is lowering the construct, and two
            // copies of it would still be lowered as something they are not.
            let both = format!("{T}\nSELECT {kw}(\"b\" AS integer) FROM \"t\";\nSELECT {kw}(\"b\" AS integer) FROM \"t\" WHERE true;");
            assert_eq!(unsupported(&both, source), kw);
            // Wherever it sits, a subquery's WHERE included.
            let nested = format!(
                "{T}\nSELECT \"id\" FROM \"t\" WHERE \"id\" IN (SELECT \"id\" FROM \"t\" WHERE {kw}(\"b\" AS integer) = 1);\nSELECT \"id\" FROM \"t\";"
            );
            assert_eq!(unsupported(&nested, source), kw);
        }
    }
}

#[test]
fn both_postgres_spellings_of_a_cast_still_lower() {
    // The control: only the foreign kinds are refused.
    let src = format!("{T}\nSELECT CAST(\"b\" AS integer) FROM \"t\";\nSELECT \"b\"::integer FROM \"t\";");
    for source in SOURCES {
        lower_with(&src, source).unwrap_or_else(|e| panic!("{source:?}: {e}"));
    }
}

#[test]
fn an_ambiguous_bare_column_names_itself_and_the_tables_that_declare_it() {
    // Two in-scope tables declare `x`; the seeded catalog says so, and the refusal says which.
    let src = r#"create table "t" ("id" INTEGER, "x" INTEGER, unique ("id"));
create table "u" ("id" INTEGER, "x" INTEGER, "y" INTEGER, unique ("id"));
create table "v" ("id" INTEGER, unique ("id"));
SELECT x FROM t a, u, v WHERE a.id = u.id AND u.id = v.id;
SELECT a.x FROM t a, u, v WHERE a.id = u.id AND u.id = v.id;"#;
    let e = lower_with(src, CatalogSource::InferredSeeded).expect_err("ambiguous").to_string();
    // `v` does not declare `x`, so it is not a candidate; an alias is shown as the FROM clause binds it.
    assert_eq!(e, "ambiguous unqualified column x (declared by t a, u)");
}

#[test]
fn without_a_catalog_the_refusal_names_every_table_in_scope() {
    // Nothing is declared, so nothing says which of the two owns `x`.
    let src = "SELECT x FROM t, u v;\nSELECT 1 FROM t, u v;";
    let e = lower_with(src, CatalogSource::Inferred).expect_err("ambiguous").to_string();
    assert_eq!(e, "ambiguous unqualified column x (no catalog says which of t, u v declares it)");
}
