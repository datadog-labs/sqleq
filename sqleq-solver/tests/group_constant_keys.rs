// Unless explicitly stated otherwise all files in this repository are licensed under the
// Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/).
// Copyright 2026-Present Datadog, Inc.

//! A `GROUP BY` key that a filter holds to one value splits no group: `GROUP BY a, b WHERE a = 1`
//! has the groups of `GROUP BY b WHERE a = 1`. Each proved pair is also evaluated on random
//! databases that keep the schema; each pair that is not proved differs in Postgres.

mod support;

use serde_json::Value;
use support::*;

/// `t(k INTEGER NOT NULL, a INTEGER, b INTEGER, c INTEGER)`, key `(k)`.
fn schemas() -> Vec<Value> {
    vec![table("t", &["INTEGER", "INTEGER", "INTEGER", "INTEGER"], &[false, true, true, true], &[&[0]])]
}

fn int(i: u32) -> Value {
    col(i, "INTEGER")
}

fn one() -> Value {
    lit("1", "INTEGER")
}

fn eq(a: Value, b: Value) -> Value {
    pred("=", vec![a, b])
}

fn count() -> Value {
    agg("COUNT", "INTEGER", vec![])
}

/// `SELECT <keys…>, COUNT(*) FROM t WHERE cond GROUP BY <keys>` (the group's own output).
fn grouped(cond: Value, keys: &[u32]) -> Value {
    group(filter(scan(0), cond), keys.iter().map(|k| int(*k)).collect(), vec![count()])
}

fn proves(a: Value, b: Value) {
    assert!(proved(&pair(schemas(), a.clone(), b.clone())), "not proved:\n{a}\n{b}");
    agrees(&schemas(), &a, &b);
}

fn refutes(a: Value, b: Value) {
    assert!(!proved(&pair(schemas(), a.clone(), b.clone())), "proved:\n{a}\n{b}");
}

#[test]
fn a_key_a_filter_holds_constant_splits_no_group() {
    // `SELECT b, COUNT(*) … WHERE a = 1 GROUP BY a, b` against `… GROUP BY b`.
    let a = project(grouped(eq(int(1), one()), &[1, 2]), vec![int(1), int(2)]);
    let b = grouped(eq(int(1), one()), &[2]);
    proves(a, b);
}

#[test]
fn a_constant_key_can_be_projected_as_the_constant() {
    // `SELECT a, b, COUNT(*) … WHERE a = 1 GROUP BY a, b` against `SELECT 1, b, COUNT(*) … GROUP BY b`.
    let a = grouped(eq(int(1), one()), &[1, 2]);
    let b = project(grouped(eq(int(1), one()), &[2]), vec![one(), int(0), int(1)]);
    proves(a, b);
}

#[test]
fn a_filter_on_a_key_moves_past_the_aggregate() {
    // `SELECT a, COUNT(*) FROM t GROUP BY a HAVING a = 1` against `… WHERE a = 1 GROUP BY a`.
    let above = filter(grouped(lit("TRUE", "BOOLEAN"), &[1]), eq(int(0), one()));
    let below = grouped(eq(int(1), one()), &[1]);
    proves(above, below);
}

#[test]
fn a_key_no_filter_holds_still_splits_groups() {
    // Differs where two rows with c = 1 have different a: A has two groups, B one.
    let a = project(grouped(eq(int(3), one()), &[1, 2]), vec![int(1), int(2)]);
    let b = grouped(eq(int(3), one()), &[2]);
    refutes(a, b);
}
