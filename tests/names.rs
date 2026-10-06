// Unless explicitly stated otherwise all files in this repository are licensed under the
// Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/).
// Copyright 2026-Present Datadog, Inc.

//! Names that two different relations share after a rewrite.
//!
//! A schema qualifier is part of a table's name. Each test below is a pair that is **not** equivalent
//! in Postgres and that used to lower to one query, or to settle as `reflexive`, because a name was
//! compared without its qualifier. Controls beside them keep the shapes that are one name lowering
//! as before. They do not run a prover; they pin the lowering.

use serde_json::Value;
use sqleq_frontend::{lower_with, reflexive, CatalogSource, FrontendError};

fn lowered(src: &str, catalog: CatalogSource) -> Value {
    lower_with(src, catalog).unwrap_or_else(|e| panic!("expected Ok, got {e}"))
}

fn identical(src: &str, catalog: CatalogSource) -> bool {
    let v = lowered(src, catalog);
    v["queries"][0] == v["queries"][1]
}

fn refused(src: &str, needle: &str) {
    match lower_with(src, CatalogSource::Declared) {
        Err(FrontendError::Unsupported(m) | FrontendError::Schema(m)) => {
            assert!(m.contains(needle), "refused for {m:?}, expected {needle:?}")
        }
        Err(e) => panic!("expected a refusal mentioning {needle:?}, got {e}"),
        Ok(_) => panic!("expected a refusal mentioning {needle:?}, but it lowered"),
    }
}

// --- `strip_schema` decides for the pair, not per query -------------------------------------------

/// `s1.t` and `s2.t` are two tables. Each query qualifies its one table consistently, so a guard that
/// looks at one query at a time strips both to `t`, and the pair becomes one query.
#[test]
fn two_qualifiers_of_one_bare_name_across_the_pair_are_two_tables() {
    let pair = "create table s1.t (a INTEGER);\ncreate table s2.t (a INTEGER);\n\
                SELECT a FROM s1.t;\nSELECT a FROM s2.t;";
    assert!(!reflexive(pair), "s1.t and s2.t settled as one query");
    for catalog in [CatalogSource::Declared, CatalogSource::InferredSeeded, CatalogSource::Inferred] {
        assert!(!identical(pair, catalog), "s1.t and s2.t lowered to one scan under {catalog:?}");
    }
    // A DDL that declares the bare name: neither side is the declared table any more.
    refused("create table t (a INTEGER);\nSELECT a FROM s1.t;\nSELECT a FROM s2.t;", "unknown table");
    // Nor a qualifier on one side only: `t` and `s2.t` are not known to be one table.
    refused("create table t (a INTEGER);\nSELECT a FROM t;\nSELECT a FROM s2.t;", "unknown table");
}

/// A quoted qualifier keeps its case, so `"S1".t` and `s1.t` are two schemas' tables.
#[test]
fn qualifiers_are_compared_under_postgres_folding() {
    let pair = "create table t (a INTEGER);\nSELECT a FROM \"S1\".t;\nSELECT a FROM s1.t;";
    assert!(!reflexive(pair), "\"S1\".t and s1.t settled as one query");
    refused(pair, "unknown table");
}

/// Control: one qualifier, however it is spelled, still strips on both sides, which is what the
/// rewrite is for.
#[test]
fn one_qualifier_on_both_sides_still_strips() {
    // Lowered at all: the declared table is the bare `t`.
    lowered("create table t (a INTEGER);\nSELECT a FROM s1.t;\nSELECT a FROM \"s1\".T WHERE a > 0;", CatalogSource::Declared);
    assert!(identical("create table t (a INTEGER);\nSELECT a FROM s1.t;\nSELECT a FROM S1.t;", CatalogSource::Declared));
}
