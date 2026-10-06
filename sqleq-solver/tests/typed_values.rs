// Unless explicitly stated otherwise all files in this repository are licensed under the
// Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/).
// Copyright 2026-Present Datadog, Inc.

//! A value keeps its SQL type: integer and decimal division are different functions, `2` and
//! `2.0` (and `2.0` and `2.00`) are different values though `=` calls them equal, constants are
//! compared exactly, `TRUE` is not `1`, and strings are not ordered by bytes. Each pair marked
//! "not equivalent" was proved before.

mod support;

use serde_json::Value;
use sqleq_solver::ir::TranslateError;
use sqleq_solver::prove::Verdict;
use support::*;

/// `t(id INTEGER, a <ty>)`.
fn t_with(ty: &str) -> Vec<Value> {
    vec![table("t", &["INTEGER", ty], &[true, true], &[])]
}

fn select_where(target: Vec<Value>, cond: Value) -> Value {
    project(filter(scan(0), cond), target)
}

fn a(ty: &str) -> Value {
    col(1, ty)
}

#[test]
fn integer_and_decimal_division_are_different_functions() {
    // t = {(1, 2), (2, 3)}: 3 / 2.0 = 1.5 > 1, but 3 / 2 = 1.
    let over = |divisor: Value, ty: &str| {
        let cond = pred(">", vec![call("/", ty, vec![a("INTEGER"), divisor]), lit("1", "INTEGER")]);
        distinct(select_where(vec![col(0, "INTEGER")], cond), &["INTEGER"])
    };
    let v = pair(t_with("INTEGER"), over(lit("2.0", "REAL"), "REAL"), over(lit("2", "INTEGER"), "INTEGER"));
    assert!(!proved(&v), "{v:?}");
}

#[test]
fn avg_is_not_integer_division() {
    // t = {(1, 1), (2, 2)}: avg is 1.5, sum / count is 1.
    let avg = group(project(scan(0), vec![a("INTEGER")]), vec![], vec![agg("AVG", "INTEGER", vec![col(0, "INTEGER")])]);
    let sum_count = project(
        group(
            project(scan(0), vec![a("INTEGER"), a("INTEGER")]),
            vec![],
            vec![agg("SUM", "INTEGER", vec![col(0, "INTEGER")]), agg("COUNT", "INTEGER", vec![col(1, "INTEGER")])],
        ),
        vec![call("/", "INTEGER", vec![col(0, "INTEGER"), col(1, "INTEGER")])],
    );
    let v = pair(t_with("INTEGER"), distinct(avg, &["INTEGER"]), distinct(sum_count, &["INTEGER"]));
    assert!(!proved(&v), "{v:?}");
}

#[test]
fn an_equality_with_a_decimal_does_not_make_an_integer_one() {
    // t = {(1, 2)}: `a = 2.0` holds, but `a` is still the integer 2.
    let eq_two = || pred("=", vec![a("INTEGER"), lit("2.0", "REAL")]);
    let v = pair(
        t_with("INTEGER"),
        select_where(vec![call("/", "INTEGER", vec![a("INTEGER"), lit("3", "INTEGER")])], eq_two()),
        select_where(vec![call("/", "REAL", vec![lit("2.0", "REAL"), lit("3", "INTEGER")])], eq_two()),
    );
    assert!(!proved(&v), "{v:?}");
    // The same through a cast, whose output type matches: '2' against '2.0'.
    for text in [|x: Value| cast("VARCHAR", x), |x: Value| call("QCAST0", "VARCHAR", vec![x])] {
        let v = pair(t_with("INTEGER"), select_where(vec![text(a("INTEGER"))], eq_two()), select_where(vec![text(lit("2.0", "REAL"))], eq_two()));
        assert!(!proved(&v), "{v:?}");
    }
}

#[test]
fn an_equality_between_decimals_does_not_make_them_one_value() {
    // `numeric` keeps its scale: on a = 2.00, `a = 2.0` holds, and `CAST(a AS TEXT)` is '2.00'.
    let eq_two = || pred("=", vec![a("REAL"), lit("2.0", "REAL")]);
    let v = pair(
        t_with("REAL"),
        select_where(vec![cast("VARCHAR", a("REAL"))], eq_two()),
        select_where(vec![cast("VARCHAR", lit("2.0", "REAL"))], eq_two()),
    );
    assert!(!proved(&v), "{v:?}");
    // Nor two columns: a = 2.0, b = 2.00.
    let schemas = vec![table("t", &["INTEGER", "REAL", "REAL"], &[true, true, true], &[])];
    let a_eq_b = || pred("=", vec![col(1, "REAL"), col(2, "REAL")]);
    let v = pair(
        schemas,
        select_where(vec![cast("VARCHAR", col(1, "REAL"))], a_eq_b()),
        select_where(vec![cast("VARCHAR", col(2, "REAL"))], a_eq_b()),
    );
    assert!(!proved(&v), "{v:?}");
}

#[test]
fn numeric_equality_still_compares_values() {
    // `a = 2.0` and `a = 2.00` are one filter, and so are `a = 2` and `a = 2.0` on a decimal.
    let ids = |rhs: Value| select_where(vec![col(0, "INTEGER")], pred("=", vec![a("REAL"), rhs]));
    assert!(proved(&pair(t_with("REAL"), ids(lit("2.0", "REAL")), ids(lit("2.00", "REAL")))));
    assert!(proved(&pair(t_with("REAL"), ids(lit("2", "INTEGER")), ids(lit("2.0", "REAL")))));
    assert!(!proved(&pair(t_with("REAL"), ids(lit("2.0", "REAL")), ids(lit("2.5", "REAL")))));
    // And an equality commutes.
    let schemas = vec![table("t", &["INTEGER", "REAL", "REAL"], &[true, true, true], &[])];
    let eq = |x: u32, y: u32| select_where(vec![col(0, "INTEGER")], pred("=", vec![col(x, "REAL"), col(y, "REAL")]));
    assert!(proved(&pair(schemas, eq(1, 2), eq(2, 1))));
}

#[test]
fn an_integer_and_a_decimal_constant_are_different_values() {
    // t = {(1, 2)}: '1.0' against '1'.
    let text_of = |x: Value| distinct(project(scan(0), vec![cast("VARCHAR", x)]), &["VARCHAR"]);
    let v = pair(t_with("INTEGER"), text_of(lit("1.0", "REAL")), text_of(lit("1", "INTEGER")));
    assert!(!proved(&v), "{v:?}");
    // And '1.0' against '1.00'.
    let v = pair(t_with("INTEGER"), text_of(lit("1.0", "REAL")), text_of(lit("1.00", "REAL")));
    assert!(!proved(&v), "{v:?}");
    // The same decimal spelled two ways is one value.
    assert!(proved(&pair(t_with("INTEGER"), text_of(lit("1.50", "REAL")), text_of(lit("01.50", "REAL")))));
}

#[test]
fn a_literal_that_is_not_its_types_value_is_refused() {
    // `2e0`, `1e-5` and `9223372036854775808` are not `bigint` literals (Postgres reads them as
    // numeric), so typed INTEGER they denote no INTEGER constant; read through a float they were
    // 2, 0 and i64::MAX.
    for text in ["2e0", "1e-5", "9223372036854775808"] {
        let v = pair(
            t_with("INTEGER"),
            project(scan(0), vec![call("/", "INTEGER", vec![a("INTEGER"), lit(text, "INTEGER")])]),
            project(scan(0), vec![call("/", "INTEGER", vec![a("INTEGER"), lit("2", "INTEGER")])]),
        );
        assert_eq!(v, Verdict::Refused(TranslateError::Literal("INTEGER".into())), "{text}");
    }
}

#[test]
fn constants_are_compared_exactly() {
    // 2^53 + 1 is not 2^53, and numeric 0.10000000000000001 is greater than 0.1, though each pair
    // is one double.
    let ids = |cond: Value| select_where(vec![col(0, "INTEGER")], cond);
    let all = project(scan(0), vec![col(0, "INTEGER")]);
    let eq = pred("=", vec![lit("9007199254740993", "INTEGER"), lit("9007199254740992", "INTEGER")]);
    assert!(!proved(&pair(t_with("INTEGER"), ids(eq), all)));
    let gt = pred(">", vec![lit("0.10000000000000001", "REAL"), lit("0.1", "REAL")]);
    assert!(!proved(&pair(t_with("REAL"), ids(gt), ids(lit("FALSE", "BOOLEAN")))));
}

#[test]
fn true_is_not_the_integer_one() {
    let select = |x: Value| project(scan(0), vec![x]);
    let v = pair(t_with("INTEGER"), select(lit("TRUE", "BOOLEAN")), select(lit("1", "INTEGER")));
    assert!(!proved(&v), "{v:?}");
    let v = pair(
        t_with("INTEGER"),
        distinct(select(lit("TRUE", "BOOLEAN")), &["BOOLEAN"]),
        distinct(select(lit("1", "INTEGER")), &["INTEGER"]),
    );
    assert!(!proved(&v), "{v:?}");
    // Nor as an argument: `to_json(TRUE)` is 'true' and `to_json(1)` is '1'.
    let to_json = |x: Value| select(call("TO_JSON", "VARBINARY", vec![x]));
    let v = pair(t_with("INTEGER"), to_json(lit("TRUE", "BOOLEAN")), to_json(lit("1", "INTEGER")));
    assert!(!proved(&v), "{v:?}");
}

#[test]
fn strings_are_not_ordered_by_bytes() {
    // Under en_US.utf8, 'B' < 'a' is false; only the C collation orders by bytes.
    let ids = |cond: Value| select_where(vec![col(0, "INTEGER")], cond);
    let s = || a("VARCHAR");
    let is_b = || pred("=", vec![s(), lit("B", "VARCHAR")]);
    let v = pair(t_with("VARCHAR"), ids(pred("AND", vec![is_b(), pred("<", vec![s(), lit("a", "VARCHAR")])])), ids(is_b()));
    assert!(!proved(&v), "{v:?}");
    let v = pair(t_with("VARCHAR"), ids(pred("<", vec![lit("a", "VARCHAR"), lit("B", "VARCHAR")])), ids(lit("FALSE", "BOOLEAN")));
    assert!(!proved(&v), "{v:?}");
    // The same order comparison on both sides still matches.
    let lt = || pred("<", vec![s(), lit("a", "VARCHAR")]);
    assert!(proved(&pair(t_with("VARCHAR"), ids(pred("AND", vec![is_b(), lt()])), ids(pred("AND", vec![lt(), is_b()])))));
}
