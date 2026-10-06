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
use sqleq_frontend::{lower_with, CatalogSource};

fn lower_in(src: &str, mode: CatalogSource) -> Value {
    lower_with(src, mode).unwrap_or_else(|e| panic!("expected Ok, got {e}\n{src}"))
}

/// The pair lowered against `ddl`, declared catalog.
fn lower(ddl: &str, q0: &str, q1: &str) -> Value {
    lower_in(&format!("{ddl}\n{q0};\n{q1};"), CatalogSource::Declared)
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