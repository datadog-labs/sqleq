// Unless explicitly stated otherwise all files in this repository are licensed under the
// Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/).
// Copyright 2026-Present Datadog, Inc.

//! The opaque VARBINARY stands for types whose `=` is identity (`bytea`, `uuid`, `int[]`) and for
//! types whose `=` is not (`double precision`: `0 = -0`; `jsonb`, `numeric[]`: `2.0 = 2.00`). The
//! emitted schema says which opaque columns are of the first kind, in `opaque_identity`, parallel
//! to `types`, so that `sqleq-solver` can read `=` on them as identity. Both readers of a `CREATE
//! TABLE` say it, the one for the declared catalog and the one for raw Postgres DDL, and a schema
//! with no such column is emitted without the key, exactly as before.

use serde_json::Value;
use sqleq_frontend::{lower_sql, lower_with, lower_with_ddl, CatalogSource};

const PAIR: &str = "SELECT \"id\" FROM \"t\";\nSELECT \"id\" FROM \"t\" WHERE \"id\" = 1;";

/// `t`'s emitted `types` and `opaque_identity` (absent: `None`), read through raw DDL when `raw`.
fn schema_of(column_type: &str, raw: bool) -> (Vec<String>, Option<Vec<bool>>) {
    let ddl = format!("create table \"t\" (\"id\" INTEGER, \"c\" {column_type});");
    let v = if raw {
        lower_with_ddl(PAIR, &ddl, CatalogSource::Declared)
    } else {
        lower_sql(&format!("{ddl}\n{PAIR}"))
    }
    .unwrap_or_else(|e| panic!("{column_type}: {e}"));
    let s = &v["schemas"][0];
    let types = s["types"].as_array().unwrap().iter().map(|t| t.as_str().unwrap().to_string()).collect();
    let flags = s.get("opaque_identity").map(|f| f.as_array().unwrap().iter().map(|b| b.as_bool().unwrap()).collect());
    (types, flags)
}

#[test]
fn an_opaque_column_whose_equality_is_identity_is_listed() {
    let declared = ["BYTEA", "INTEGER[]", "BIGINT[]", "TEXT[]", "VARCHAR(8)[]", "BOOLEAN[]", "DATE[]", "TIMESTAMP[]", "BYTEA[]", "INT ARRAY"];
    let raw_only = ["uuid", "money", "name", "int4range", "int8range", "daterange", "uuid[]", "timestamptz[]", "int4range[]"];
    for (ty, raw) in declared.iter().map(|t| (*t, false)).chain(declared.iter().chain(&raw_only).map(|t| (*t, true))) {
        let (types, flags) = schema_of(ty, raw);
        assert_eq!(types, ["INTEGER", "VARBINARY"], "{ty} (raw DDL: {raw})");
        assert_eq!(flags, Some(vec![false, true]), "{ty} (raw DDL: {raw})");
    }
}

#[test]
fn an_opaque_column_whose_equality_is_not_identity_is_not() {
    // Each has two values `=` calls equal that print differently, or no `=` worth the name.
    let both = ["DOUBLE PRECISION", "REAL", "NUMERIC[]", "DOUBLE PRECISION[]", "INTERVAL[]", "JSONB[]", "TIME WITH TIME ZONE"];
    let raw_only = ["jsonb", "numrange", "tsrange", "box", "point", "json", "inet", "mood", "public.uuid"];
    for (ty, raw) in both.iter().map(|t| (*t, false)).chain(both.iter().chain(&raw_only).map(|t| (*t, true))) {
        let (types, flags) = schema_of(ty, raw);
        assert_eq!(types, ["INTEGER", "VARBINARY"], "{ty} (raw DDL: {raw})");
        assert_eq!(flags, None, "{ty} (raw DDL: {raw})");
    }
}

#[test]
fn a_column_that_is_not_opaque_is_not_listed() {
    // INTEGER's `=` is identity too, but its IR type already says so; `uuid` in a declared `CREATE
    // TABLE` keeps its own name.
    for (ty, raw) in [("INTEGER", false), ("TEXT", true), ("NUMERIC", true), ("uuid", false)] {
        assert_eq!(schema_of(ty, raw).1, None, "{ty} (raw DDL: {raw})");
    }
}

#[test]
fn the_flags_are_parallel_to_the_columns_system_columns_included() {
    let ddl = "create table \"t\" (\"id\" INTEGER, \"h\" BYTEA, \"f\" DOUBLE PRECISION, \"u\" BYTEA);";
    let src = format!("{ddl}\nSELECT \"h\", \"ctid\" FROM \"t\";\nSELECT \"u\", \"ctid\" FROM \"t\";");
    for mode in [CatalogSource::Declared, CatalogSource::InferredSeeded] {
        let v: Value = lower_with(&src, mode).unwrap();
        let s = &v["schemas"][0];
        assert_eq!(s["types"].as_array().unwrap().len(), 5);
        assert_eq!(s["opaque_identity"], serde_json::json!([false, true, false, true, false]));
    }
    // Inferred from the queries alone, nothing is known to be identity.
    let v: Value = lower_with(&src, CatalogSource::Inferred).unwrap();
    assert!(v["schemas"].as_array().unwrap().iter().all(|s| s.get("opaque_identity").is_none()), "{v}");
}
