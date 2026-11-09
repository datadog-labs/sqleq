// Unless explicitly stated otherwise all files in this repository are licensed under the
// Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/).
// Copyright 2026-Present Datadog, Inc.

//! Order comparisons: what the encoding may assume about `<` in each [`Order`] domain. Each proved
//! pair is also evaluated on random databases that keep the schema; each pair that is not proved
//! differs in Postgres, on the value its comment names.
//!
//! [`Order`]: sqleq::solver::uterm::Order

mod support;

use serde_json::Value;
use support::*;

/// `t(k INTEGER NOT NULL, a INTEGER, b INTEGER, v VARCHAR, w VARCHAR, d REAL)`, key `(k)`.
fn schemas() -> Vec<Value> {
    vec![table(
        "t",
        &["INTEGER", "INTEGER", "INTEGER", "VARCHAR", "VARCHAR", "REAL"],
        &[false, true, true, true, true, true],
        &[&[0]],
    )]
}

const TYPES: [&str; 6] = ["INTEGER", "INTEGER", "INTEGER", "VARCHAR", "VARCHAR", "REAL"];

fn c(i: u32) -> Value {
    col(i, TYPES[i as usize])
}

fn int(n: i64) -> Value {
    lit(&n.to_string(), "INTEGER")
}

fn text(s: &str) -> Value {
    lit(s, "VARCHAR")
}

fn cmp(op: &str, a: Value, b: Value) -> Value {
    pred(op, vec![a, b])
}

fn and(parts: Vec<Value>) -> Value {
    pred("AND", parts)
}

fn falsity() -> Value {
    lit("FALSE", "BOOLEAN")
}

/// `SELECT k FROM t WHERE cond`.
fn rows(cond: Value) -> Value {
    project(filter(scan(0), cond), vec![c(0)])
}

fn proves(a: Value, b: Value) {
    let (a, b) = (rows(a), rows(b));
    assert!(proved(&pair(schemas(), a.clone(), b.clone())), "not proved:\n{a}\n{b}");
    agrees(&schemas(), &a, &b);
}

fn refutes(a: Value, b: Value) {
    let (a, b) = (rows(a), rows(b));
    assert!(!proved(&pair(schemas(), a.clone(), b.clone())), "proved:\n{a}\n{b}");
}

#[test]
fn an_integer_point_span_is_an_equality() {
    // `1 <= a AND a <= 1`, the span an index lookup writes for `a = 1`.
    proves(and(vec![cmp("<=", int(1), c(1)), cmp("<=", c(1), int(1))]), cmp("=", c(1), int(1)));
}

#[test]
fn a_text_point_span_is_an_equality() {
    // Antisymmetry needs no order between constants, only an `=` that is identity.
    proves(and(vec![cmp(">=", c(3), text("a")), cmp("<=", c(3), text("a"))]), cmp("=", c(3), text("a")));
}

#[test]
fn integers_are_discrete() {
    proves(cmp(">", c(1), int(0)), cmp(">=", c(1), int(1)));
}

#[test]
fn a_tighter_integer_bound_implies_a_looser_one() {
    proves(and(vec![cmp(">", c(1), int(1)), cmp(">", c(1), int(0))]), cmp(">", c(1), int(1)));
}

#[test]
fn an_empty_integer_range_is_false() {
    proves(and(vec![cmp("<", c(1), int(1)), cmp(">", c(1), int(1))]), falsity());
}

#[test]
fn a_cycle_of_text_comparisons_is_false() {
    // Transitivity and asymmetry, which hold under every collation.
    proves(and(vec![cmp("<", c(3), c(4)), cmp("<", c(4), text("b")), cmp(">=", c(3), text("b"))]), falsity());
}

#[test]
fn two_columns_each_at_most_the_other_are_equal() {
    // Both sides are UNKNOWN, and filtered out, where either column is NULL.
    proves(and(vec![cmp("<=", c(1), c(2)), cmp("<=", c(2), c(1))]), cmp("=", c(1), c(2)));
}

#[test]
fn a_column_is_at_most_itself_unless_null() {
    proves(cmp(">=", c(1), c(1)), pred("IS NOT NULL", vec![c(1)]));
    proves(cmp("<=", c(0), c(0)), lit("TRUE", "BOOLEAN"));
}

#[test]
fn text_is_not_discrete() {
    // Differs at `v = 'aa'`, which is above `'a'` and below `'b'` in every collation.
    refutes(cmp(">", c(3), text("a")), cmp(">=", c(3), text("b")));
}

#[test]
fn text_constants_are_not_ordered() {
    // `'a' < 'B'` under `en_US`, not under `C`; and `'aa'` lies between `'a'` and `'b'`.
    refutes(and(vec![cmp(">", c(3), text("a")), cmp("<", c(3), text("b"))]), falsity());
    refutes(cmp(">", c(3), text("b")), and(vec![cmp(">", c(3), text("b")), cmp(">", c(3), text("a"))]));
}

#[test]
fn numerics_are_not_discrete() {
    // Differs at `d = 1.5`.
    refutes(cmp(">", c(5), lit("1", "REAL")), cmp(">=", c(5), lit("2", "REAL")));
}

#[test]
fn integers_are_not_denser_than_they_are() {
    // Differs at `a = 2`.
    refutes(cmp(">", c(1), int(1)), cmp(">=", c(1), int(3)));
}

#[test]
fn a_column_span_is_not_null_safe() {
    // Differs where `a` and `b` are both NULL: the span is UNKNOWN, `IS NOT DISTINCT FROM` TRUE.
    refutes(and(vec![cmp("<=", c(1), c(2)), cmp("<=", c(2), c(1))]), pred("IS NOT DISTINCT FROM", vec![c(1), c(2)]));
}
