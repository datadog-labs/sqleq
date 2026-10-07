// Unless explicitly stated otherwise all files in this repository are licensed under the
// Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/).
// Copyright 2026-Present Datadog, Inc.

//! Aggregates that add in floating point (issue #85).
//!
//! Float addition rounds, so it is not associative: `(1e20 + 1) + -1e20` is `0` and
//! `(-1e20 + 1e20) + 1` is `1`. Over `real` or `double precision`, `sum`, `avg` and the `stddev` and
//! `var` family add the values in the order the rows reach the aggregate, and `corr`, `covar_*` and
//! the `regr_*` other than `regr_count` compute in floating point whatever they are given. Each
//! refused pair here is not equivalent in Postgres: it feeds one bag in two orders, and used to lower
//! to IR a prover proves equal. Over `numeric` and the integers the sums are exact, and `min`, `max`
//! and `count` do not add, so those still lower.

use sqleq_frontend::{lower_sql, FrontendError};

const T: &str = r#"create table "t" ("y" INTEGER, "x" DOUBLE PRECISION, "r" REAL, "n" NUMERIC, "i" INTEGER);
create table "u" ("y" INTEGER, "x" DOUBLE PRECISION, "r" REAL, "n" NUMERIC, "i" INTEGER);"#;

fn pair(q0: &str, q1: &str) -> String {
    format!("{T}\n{q0};\n{q1};")
}

fn refused(q0: &str, q1: &str) {
    match lower_sql(&pair(q0, q1)) {
        Err(FrontendError::Unsupported(m)) => assert!(m.contains("adds in floating point"), "refused for {m:?}"),
        Err(e) => panic!("expected a refusal, got {e}\n{q0}\n{q1}"),
        Ok(_) => panic!("expected a refusal, but it lowered\n{q0}\n{q1}"),
    }
}

fn lowers(q0: &str, q1: &str) {
    if let Err(e) = lower_sql(&pair(q0, q1)) {
        panic!("expected Ok, got {e}\n{q0}\n{q1}");
    }
}

/// `SELECT <agg> FROM (t UNION ALL u)` against the branches swapped: one bag, two orders.
fn swapped(agg: &str) -> (String, String) {
    let q = |a: &str, b: &str| {
        format!(r#"SELECT {agg} FROM (SELECT * FROM "{a}" UNION ALL SELECT * FROM "{b}") AS "s""#)
    };
    (q("t", "u"), q("u", "t"))
}

#[test]
fn a_float_sum_is_refused_where_the_order_of_its_inputs_can_change() {
    // On t = {(1e20), (1)}, u = {(-1e20)}: A adds 1e20, 1, -1e20 and yields 0; B yields 1.
    for agg in [
        r#"SUM("x")"#,
        r#"AVG("x")"#,
        r#"SUM("r")"#,
        r#"SUM(DISTINCT "x")"#,
        r#"SUM("x") FILTER (WHERE "y" > 0)"#,
        r#"SUM(coalesce("x", 0))"#,
        r#"SUM("x" * 2)"#,
        r#"SUM(CAST("n" AS DOUBLE PRECISION))"#,
        r#"stddev("x")"#,
        r#"var_pop("r")"#,
        r#"variance("x")"#,
        // Declared over double precision only, so an integer argument is converted to one first.
        r#"corr("x", "x")"#,
        r#"corr("i", "i")"#,
        r#"regr_slope("i", "i")"#,
        r#"covar_pop("n", "n")"#,
    ] {
        let (q0, q1) = swapped(agg);
        refused(&q0, &q1);
    }
    // Exact sums, and aggregates that do not add, are functions of the bag and still lower.
    for agg in [
        r#"SUM("n")"#,
        r#"AVG("i")"#,
        r#"SUM("i")"#,
        r#"MIN("x")"#,
        r#"MAX("x")"#,
        r#"COUNT("x")"#,
        r#"SUM(CAST("x" AS NUMERIC))"#,
        r#"regr_count("x", "x")"#,
    ] {
        let (q0, q1) = swapped(agg);
        lowers(&q0, &q1);
    }
}

#[test]
fn a_float_sum_over_a_sorted_subquery_is_not_one_plan() {
    // The lowering drops both ORDER BYs, so these were one plan, proved by both provers: on
    // t = {(1, 1e20), (2, -1e20), (3, 1)}, A yields 1 and B yields 0.
    refused(
        r#"SELECT SUM("x") FROM (SELECT "x" FROM "t" ORDER BY "y") AS "s""#,
        r#"SELECT SUM("x") FROM (SELECT "x" FROM "t" ORDER BY "y" DESC) AS "s""#,
    );
    // Against the same sum in scan order.
    refused(r#"SELECT SUM("x") FROM (SELECT "x" FROM "t" ORDER BY "y") AS "s""#, r#"SELECT SUM("x") FROM "t""#);
    // Over numeric the sorts change nothing, and the pair is the one plan it always was.
    let v = lower_sql(&pair(
        r#"SELECT SUM("n") FROM (SELECT "n" FROM "t" ORDER BY "y") AS "s""#,
        r#"SELECT SUM("n") FROM (SELECT "n" FROM "t" ORDER BY "y" DESC) AS "s""#,
    ))
    .unwrap();
    assert_eq!(v["queries"][0], v["queries"][1]);
}
