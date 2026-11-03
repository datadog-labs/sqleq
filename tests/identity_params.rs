// Unless explicitly stated otherwise all files in this repository are licensed under the
// Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/).
// Copyright 2026-Present Datadog, Inc.

//! A parameter compared only with columns of one opaque type whose `=` is identity is listed in
//! `identity_params` (`src/param_types.rs`), for `sqleq-solver` to read its `=` as identity too.
//! Postgres types an untyped parameter at the column it is compared with, so such a parameter holds
//! a value of that type. These pin the lowering and run no prover; the pairs under
//! `tests/pairs/types/` run them.

use serde_json::Value;
use sqleq_frontend::{lower_with, CatalogSource};

const T: &str = r#"create table "t" ("id" INTEGER PRIMARY KEY, "u" uuid, "b" bytea, "j" jsonb, "f" DOUBLE PRECISION, "v" uuid);"#;

fn lowered(q0: &str, q1: &str) -> Value {
    let src = format!("{T}\n{q0};\n{q1};");
    lower_with(&src, CatalogSource::InferredSeeded).unwrap_or_else(|e| panic!("did not lower: {e}\n{q0}\n{q1}"))
}

fn marked(q0: &str, q1: &str) -> Vec<String> {
    match lowered(q0, q1).get("identity_params") {
        None => Vec::new(),
        Some(v) => {
            let names: Vec<String> = v.as_array().expect("a list").iter().map(|n| n.as_str().expect("a name").to_string()).collect();
            assert!(!names.is_empty(), "an empty identity_params is not emitted");
            names
        }
    }
}

#[test]
fn a_parameter_compared_with_a_uuid_or_bytea_column_is_marked() {
    let q = |c: &str| format!(r#"SELECT "id" FROM "t" WHERE "{c}" = $1"#);
    assert_eq!(marked(&q("u"), &q("u")), ["QP1"]);
    assert_eq!(marked(&q("b"), &q("b")), ["QP1"]);
    // Two columns of the one type, and the other comparisons that read `=`.
    let both = r#"SELECT "id" FROM "t" WHERE "v" <> $1 OR "u" IS NOT DISTINCT FROM $1"#;
    assert_eq!(marked(both, both), ["QP1"]);
}

#[test]
fn a_parameter_compared_with_a_column_whose_equality_is_not_identity_is_not() {
    // `jsonb` (`{"a": 1.0} = {"a": 1.00}`) and floats (`0 = -0`) have an `=` that is not identity.
    for c in ["j", "f"] {
        let q = format!(r#"SELECT "id" FROM "t" WHERE "{c}" = $1"#);
        assert!(marked(&q, &q).is_empty(), "{c}");
    }
}

#[test]
fn one_comparison_with_another_type_leaves_the_parameter_out() {
    // Within a query. (Across the two, `$1` set against `t.u` in one and `t.j` in the other is
    // refused before this, as a misaligned parameter.)
    for other in ["b", "j"] {
        let q = format!(r#"SELECT "id" FROM "t" WHERE "u" = $1 AND "{other}" = $1"#);
        // A refusal outright marks nothing either.
        if let Ok(v) = lower_with(&format!("{T}\n{q};\n{q};"), CatalogSource::InferredSeeded) {
            assert!(v.get("identity_params").is_none(), "{other}: {}", v["identity_params"]);
        }
    }
}

#[test]
fn only_the_parameters_that_qualify_are_marked() {
    let q0 = r#"SELECT "id" FROM "t" WHERE "u" = $1 AND "j" = $2"#;
    assert_eq!(marked(q0, q0), ["QP1"]);
}

#[test]
fn a_pair_without_such_a_comparison_has_no_field() {
    let q = r#"SELECT "id" FROM "t" WHERE "id" = $1"#;
    assert!(lowered(q, q).get("identity_params").is_none());
}
