// Unless explicitly stated otherwise all files in this repository are licensed under the
// Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/).
// Copyright 2026-Present Datadog, Inc.

//! The temporal types: DATE, TIME, TIMESTAMP, TIMESTAMPTZ and INTERVAL are kept apart in the IR,
//! and every crossing between two of them is a named conversion (see `src/types.rs`).
//!
//! Each "not identical" test below is a pair that is **not** equivalent in Postgres and used to
//! lower to IR that a prover proved equal, most of them to byte-identical IR. Reading dates and
//! timestamps as one integer did it: `d + 1` is the next day, not the next microsecond, and
//! `CAST(ts AS DATE)` truncates. These tests do not run a prover; they pin the lowering that keeps
//! the two sides apart, and the identical lowering that keeps the equivalent shapes provable.

use serde_json::Value;
use sqleq_frontend::{lower_with, CatalogSource, FrontendError};

const T: &str = r#"create table "t" ("id" INTEGER, "ts" TIMESTAMP, "d" DATE, "tz" TIMESTAMPTZ, "s" VARCHAR, "dur" INTERVAL, unique ("id"));"#;

fn lower(q0: &str, q1: &str, src: CatalogSource) -> Value {
    lower_with(&format!("{T}\n{q0};\n{q1};"), src).unwrap_or_else(|e| panic!("expected Ok, got {e}"))
}

/// Whether the pair lowers to byte-identical queries, in the corpus runs' mode.
fn identical(q0: &str, q1: &str) -> bool {
    let v = lower(q0, q1, CatalogSource::InferredSeeded);
    v["queries"][0] == v["queries"][1]
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

fn ops_of(q0: &str, q1: &str, src: CatalogSource) -> Vec<String> {
    let mut out = Vec::new();
    operators(&lower(q0, q1, src), &mut out);
    out
}

fn refused(q0: &str, q1: &str, needle: &str) {
    match lower_with(&format!("{T}\n{q0};\n{q1};"), CatalogSource::InferredSeeded) {
        Err(FrontendError::Unsupported(m)) => assert!(m.contains(needle), "refused for {m:?}"),
        Err(e) => panic!("expected an Unsupported refusal mentioning {needle:?}, got {e}"),
        Ok(_) => panic!("expected a refusal mentioning {needle:?}, but it lowered"),
    }
}

#[test]
fn dropping_the_date_truncation_no_longer_lowers_identically() {
    // `ts::date = $1` is a day long; `ts = $1::date` is one instant, midnight.
    assert!(!identical(
        r#"SELECT "id" FROM "t" WHERE "ts"::date = $1::date"#,
        r#"SELECT "id" FROM "t" WHERE "ts" = $1::date"#,
    ));
    assert!(!identical(
        r#"SELECT "id" FROM "t" WHERE CAST("ts" AS DATE) = "d""#,
        r#"SELECT "id" FROM "t" WHERE "ts" = "d""#,
    ));
}

#[test]
fn a_date_plus_one_is_a_day_not_a_microsecond() {
    // Over integers `x < y + 1` is `x <= y`; here `d + 1` is the next day, so the two differ for
    // any mid-day timestamp. The `+ 1` has to stay on the DATE side of the conversion.
    for src in [CatalogSource::InferredSeeded, CatalogSource::Declared] {
        let v = lower(r#"SELECT "id" FROM "t" WHERE "ts" < "d" + 1"#, r#"SELECT "id" FROM "t" WHERE "ts" <= "d""#, src);
        let conv = &v["queries"][0]["project"]["source"]["filter"]["condition"]["operand"][1];
        assert_eq!(conv["operator"], "q_conv_date_timestamp", "{src:?}: {conv}");
        assert_eq!(conv["operand"][0]["operator"], "+");
        assert_eq!(conv["operand"][0]["type"], "DATE");
    }
    assert!(!identical(r#"SELECT "id" FROM "t" WHERE "d" < "ts""#, r#"SELECT "id" FROM "t" WHERE "d" + 1 <= "ts""#));
}

#[test]
fn casts_to_different_temporal_types_are_different_functions() {
    // One shared `qcast` symbol used to stand for all four.
    assert!(!identical(r#"SELECT "s"::date FROM "t""#, r#"SELECT "s"::timestamp FROM "t""#));
    assert!(!identical(r#"SELECT "s"::date FROM "t""#, r#"SELECT "s"::integer FROM "t""#));
    let ops = ops_of(r#"SELECT "s"::date FROM "t""#, r#"SELECT "s"::timestamp FROM "t""#, CatalogSource::InferredSeeded);
    assert!(ops.contains(&"q_conv_varchar_date".to_string()), "{ops:?}");
    assert!(ops.contains(&"q_conv_varchar_timestamp".to_string()), "{ops:?}");
}

#[test]
fn a_cast_against_a_promotion_keeps_its_comparison() {
    // `<` survives the truncation (the first instant of day $1 is the promoted $1); `<=` does not.
    assert!(identical(
        r#"SELECT "id" FROM "t" WHERE CAST("ts" AS date) < $1::date"#,
        r#"SELECT "id" FROM "t" WHERE "ts" < ($1::date)::timestamp"#,
    ));
    assert!(!identical(
        r#"SELECT "id" FROM "t" WHERE CAST("ts" AS date) <= $1::date"#,
        r#"SELECT "id" FROM "t" WHERE "ts" <= ($1::date)::timestamp"#,
    ));
}

#[test]
fn a_truncated_timestamp_against_a_day_lowers_like_the_range() {
    // The equivalent shape the unsound model used to prove: now it lowers identically, soundly.
    assert!(identical(
        r#"SELECT "id" FROM "t" WHERE "ts"::date = $1"#,
        r#"SELECT "id" FROM "t" WHERE "ts" >= $1::date AND "ts" < $1::date + 1"#,
    ));
    assert!(identical(
        r#"SELECT "id" FROM "t" WHERE CAST("ts" AS DATE) = "d""#,
        r#"SELECT "id" FROM "t" WHERE "d" = CAST("ts" AS DATE)"#,
    ));
}

#[test]
fn a_truncated_timestamptz_is_not_rewritten() {
    // Postgres itself disagrees with the rewrite for TIMESTAMPTZ in some time zones (a local date
    // can happen twice), so the pair must not lower identically.
    assert!(!identical(
        r#"SELECT "id" FROM "t" WHERE "tz"::date = $1"#,
        r#"SELECT "id" FROM "t" WHERE "tz" >= $1::date AND "tz" < $1::date + 1"#,
    ));
}

#[test]
fn date_arithmetic_stays_native_and_interval_arithmetic_does_not() {
    let v = lower(r#"SELECT "d" + 1, "d" - "d" FROM "t""#, r#"SELECT "d" + 1, "d" - "d" FROM "t""#, CatalogSource::Declared);
    let target = &v["queries"][0]["project"]["target"];
    assert_eq!((target[0]["operator"].as_str(), target[0]["type"].as_str()), (Some("+"), Some("DATE")));
    assert_eq!((target[1]["operator"].as_str(), target[1]["type"].as_str()), (Some("-"), Some("INTEGER")));
    // `ts + dur - dur` is not `ts` when the interval counts months, so it must not reduce to it.
    let ops = ops_of(r#"SELECT "ts" + "dur" - "dur" FROM "t""#, r#"SELECT "ts" FROM "t""#, CatalogSource::Declared);
    assert!(ops.contains(&"q_arith_add_timestamp_interval".to_string()), "{ops:?}");
    assert!(ops.contains(&"q_arith_sub_timestamp_interval".to_string()), "{ops:?}");
    assert!(!ops.contains(&"+".to_string()) && !ops.contains(&"-".to_string()), "{ops:?}");
}

#[test]
fn temporal_types_keep_their_names_in_the_ir() {
    let v = lower(r#"SELECT "tz" FROM "t""#, r#"SELECT "tz" FROM "t""#, CatalogSource::Declared);
    let types: Vec<&str> = v["schemas"][0]["types"].as_array().unwrap().iter().filter_map(Value::as_str).collect();
    // TIMESTAMPTZ leaves as TIMESTAMP (the prover keeps its order that way); the time zone lives in
    // the names of the conversions to and from it.
    assert_eq!(types, ["INTEGER", "TIMESTAMP", "DATE", "TIMESTAMP", "VARCHAR", "INTERVAL"]);
    let ops = ops_of(r#"SELECT "id" FROM "t" WHERE "tz" > "ts""#, r#"SELECT "id" FROM "t" WHERE "tz" > "ts""#, CatalogSource::Declared);
    assert!(ops.contains(&"q_conv_timestamp_timestamptz".to_string()), "{ops:?}");
}

#[test]
fn timestamptz_never_leaves_the_frontend_under_its_own_name() {
    // Every type position, the sort collation's included: a stray TIMESTAMPTZ would be an
    // equality-only sort to QED, an unknown type to the Rust port and `ANY` to the JVM bridge.
    for src in [CatalogSource::InferredSeeded, CatalogSource::Declared] {
        let q = r#"SELECT "tz", "tz" > "ts" FROM "t" ORDER BY "tz" DESC LIMIT 5"#;
        let s = lower(q, q, src).to_string();
        assert!(!s.contains("\"TIMESTAMPTZ\""), "{src:?}: {s}");
        assert!(s.contains("q_conv_timestamp_timestamptz"), "{src:?}: the conversion keeps the name");
    }
}

#[test]
fn ordering_within_one_temporal_type_needs_no_conversion() {
    let ops = ops_of(
        r#"SELECT "id" FROM "t" WHERE "ts" >= $1 AND "ts" < $2"#,
        r#"SELECT "id" FROM "t" WHERE $1 <= "ts" AND "ts" < $2"#,
        CatalogSource::InferredSeeded,
    );
    assert!(!ops.iter().any(|o| o.starts_with("q_conv_")), "{ops:?}");
}

#[test]
fn an_update_converts_to_the_column_type_as_postgres_does() {
    // Postgres casts an assigned value to its column's type, so these store the same thing.
    assert!(identical(
        r#"UPDATE "t" SET "ts" = $1::timestamptz WHERE "id" = $2"#,
        r#"UPDATE "t" SET "ts" = ($1::timestamptz)::timestamp WHERE "id" = $2"#,
    ));
    assert!(identical(r#"UPDATE "t" SET "d" = "ts""#, r#"UPDATE "t" SET "d" = "ts"::date"#));
    // ... while a value that differs in more than its spelling still differs.
    assert!(!identical(r#"UPDATE "t" SET "ts" = "d""#, r#"UPDATE "t" SET "ts" = "d" + 1"#));
    // A value the cast rewrite cannot type is left unwrapped, so two spellings of it still match.
    assert!(identical(
        r#"UPDATE "t" SET "ts" = CASE WHEN "id" = $1 OR "id" = $2 THEN (SELECT max("ts") FROM "t") ELSE $3 END"#,
        r#"UPDATE "t" SET "ts" = CASE WHEN "id" = ANY(ARRAY[$1, $2]) THEN (SELECT max("ts") FROM "t") ELSE $3 END"#,
    ));
}

#[test]
fn relation_shapes_that_would_mix_units_are_refused() {
    refused(r#"SELECT "d" FROM "t" UNION SELECT "ts" FROM "t""#, r#"SELECT "d" FROM "t""#, "set operation over columns of type DATE and TIMESTAMP");
    refused(r#"SELECT "id" FROM "t" WHERE "ts" IN (SELECT "d" FROM "t")"#, r#"SELECT "id" FROM "t""#, "IN subquery");
    // A temporal column against an untyped parameter or a NULL is not two units; it lowers as the
    // INTEGER it replaces always did.
    let q = r#"SELECT "tz" FROM "t" UNION ALL SELECT $1 FROM "t" UNION ALL SELECT NULL FROM "t""#;
    assert!(identical(q, q));
    // The other direction is Postgres promoting the left operand, which the lowering can do.
    let ops = ops_of(
        r#"SELECT "id" FROM "t" WHERE "d" IN (SELECT "ts" FROM "t")"#,
        r#"SELECT "id" FROM "t""#,
        CatalogSource::InferredSeeded,
    );
    assert!(ops.contains(&"q_conv_date_timestamp".to_string()), "{ops:?}");
}
