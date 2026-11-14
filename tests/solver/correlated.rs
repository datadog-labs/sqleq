// Unless explicitly stated otherwise all files in this repository are licensed under the
// Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/).
// Copyright 2026-Present Datadog, Inc.

//! Correlated subqueries: a subquery reads the row of the query it sits in. Each proved pair is
//! also evaluated on random databases that keep the schema; each pair that is not proved differs
//! in Postgres.

mod support;

use serde_json::{json, Value};
use support::*;

/// `t(k INTEGER NOT NULL, fk INTEGER, a INTEGER, v VARCHAR)`, key `(k)`; `u(k INTEGER NOT NULL,
/// b INTEGER)`, key `(k)`; `w(k INTEGER, b INTEGER)`, no key. A subquery over `t` numbers its own
/// columns from 4.
fn schemas() -> Vec<Value> {
    vec![
        table("t", &["INTEGER", "INTEGER", "INTEGER", "VARCHAR"], &[false, true, true, true], &[&[0]]),
        table("u", &["INTEGER", "INTEGER"], &[false, true], &[&[0]]),
        table("w", &["INTEGER", "INTEGER"], &[true, true], &[]),
    ]
}

fn int(i: u32) -> Value {
    col(i, "INTEGER")
}

fn param(n: u32) -> Value {
    call(&format!("QP{n}"), "INTEGER", vec![lit("0", "INTEGER")])
}

fn eq(a: Value, b: Value) -> Value {
    pred("=", vec![a, b])
}

fn and(parts: Vec<Value>) -> Value {
    pred("AND", parts)
}

fn not(p: Value) -> Value {
    pred("NOT", vec![p])
}

fn in_(operand: Vec<Value>, query: Value) -> Value {
    json!({ "operator": "IN", "type": "BOOLEAN", "operand": operand, "query": query })
}

/// `SELECT <cols of t> FROM t WHERE cond`.
fn of_t(cond: Value, cols: &[u32]) -> Value {
    project(filter(scan(0), cond), cols.iter().map(|c| int(*c)).collect())
}

/// `SELECT 1 FROM <table> WHERE cond`, a subquery over `t`'s row.
fn one_from(table: usize, cond: Value) -> Value {
    project(filter(scan(table), cond), vec![lit("1", "INTEGER")])
}

fn proves(a: Value, b: Value) {
    assert!(proved(&pair(schemas(), a.clone(), b.clone())), "not proved:\n{a}\n{b}");
    agrees(&schemas(), &a, &b);
}

fn refutes(a: Value, b: Value) {
    assert!(!proved(&pair(schemas(), a.clone(), b.clone())), "proved:\n{a}\n{b}");
}

#[test]
fn a_predicate_on_the_outer_row_moves_into_and_out_of_an_exists() {
    // `WHERE EXISTS (SELECT 1 FROM w WHERE w.k = t.fk AND t.a = $1)` against
    // `WHERE t.a = $1 AND EXISTS (SELECT 1 FROM w WHERE w.k = t.fk)`.
    let matched = || eq(int(4), int(1));
    let held = || eq(int(2), param(1));
    let inside = of_t(exists(one_from(2, and(vec![matched(), held()]))), &[0]);
    let outside = of_t(and(vec![held(), exists(one_from(2, matched()))]), &[0]);
    proves(inside, outside);
}

#[test]
fn a_correlated_in_is_an_exists_of_the_equality() {
    // `WHERE t.a IN (SELECT b FROM w WHERE w.k = t.fk)` against
    // `WHERE EXISTS (SELECT 1 FROM w WHERE w.k = t.fk AND w.b = t.a)`.
    let member = of_t(in_(vec![int(2)], project(filter(scan(2), eq(int(4), int(1))), vec![int(5)])), &[0]);
    let witness = of_t(exists(one_from(2, and(vec![eq(int(4), int(1)), eq(int(5), int(2))]))), &[0]);
    proves(member, witness);
    // Their negations are not one another: `NOT IN` is UNKNOWN, so drops the row, where the
    // subquery yields a NULL `b` or `t.a` is NULL; `NOT EXISTS` keeps it.
    let not_member = of_t(not(in_(vec![int(2)], project(filter(scan(2), eq(int(4), int(1))), vec![int(5)]))), &[0]);
    let no_witness = of_t(not(exists(one_from(2, and(vec![eq(int(4), int(1)), eq(int(5), int(2))])))), &[0]);
    refutes(not_member.clone(), no_witness.clone());
    // And the random databases tell them apart, so `agrees` above checks something.
    assert!(std::panic::catch_unwind(|| agrees(&schemas(), &not_member, &no_witness)).is_err());
}

#[test]
fn a_join_on_a_key_is_an_exists() {
    // `SELECT t.k FROM t JOIN u ON u.k = t.fk` against `… WHERE EXISTS (SELECT 1 FROM u WHERE
    // u.k = t.fk)`: each row of `t` meets one row of `u` at most.
    let joined = |table: usize| {
        project(json!({ "join": { "kind": "INNER", "left": scan(0), "right": scan(table), "condition": eq(int(4), int(1)) } }), vec![int(0)])
    };
    proves(joined(1), of_t(exists(one_from(1, eq(int(4), int(1)))), &[0]));
    // Without a key a row of `t` comes back once per match.
    refutes(joined(2), of_t(exists(one_from(2, eq(int(4), int(1)))), &[0]));
}

#[test]
fn a_chain_of_joins_on_keys_is_an_exists() {
    // `sale JOIN shop ON shop.id = sale.shop_id JOIN district ON district.id = shop.district_id`
    // against `sale WHERE EXISTS (the same two joins)`: the sale's `shop_id` pins one shop, and
    // that shop's `district_id` one district (#236). The second key is tied to a column of the
    // first joined row, not of the sale.
    let schemas = |district_key: &[&[usize]]| {
        vec![
            table("sale", &["INTEGER", "INTEGER", "INTEGER"], &[false, false, true], &[&[0]]),
            table("shop", &["INTEGER", "INTEGER"], &[false, false], &[&[0]]),
            table("district", &["INTEGER", "INTEGER"], &[false, true], district_key),
        ]
    };
    let join = |left: Value, right: usize, on: Value| json!({ "join": { "kind": "INNER", "left": left, "right": scan(right), "condition": on } });
    // sale is columns 0-2, shop 3-4, district 5-6.
    let joined = project(join(join(scan(0), 1, eq(int(3), int(1))), 2, eq(int(5), int(4))), vec![int(0), int(2)]);
    // Under the EXISTS, shop is columns 3-4 and district 5-6 again, with the sale's row outside.
    let inner = project(filter(join(scan(1), 2, eq(int(5), int(4))), eq(int(3), int(1))), vec![lit("1", "INTEGER")]);
    let exists_form = project(filter(scan(0), exists(inner)), vec![int(0), int(2)]);
    let keyed = schemas(&[&[0]]);
    assert!(proved(&pair(keyed.clone(), joined.clone(), exists_form.clone())), "the chain");
    agrees(&keyed, &joined, &exists_form);
    // Without district's key a sale comes back once per matching district.
    let unkeyed = schemas(&[]);
    assert!(!proved(&pair(unkeyed.clone(), joined.clone(), exists_form.clone())), "no key on district");
    assert!(std::panic::catch_unwind(|| agrees(&unkeyed, &joined, &exists_form)).is_err());
}

#[test]
fn a_semi_join_of_a_table_with_itself_on_its_key_is_its_filter() {
    // `WHERE k IN (SELECT k FROM t WHERE a = $1)` against `WHERE a = $1`.
    let held = |c: u32| eq(int(c), param(1));
    proves(of_t(in_(vec![int(0)], project(filter(scan(0), held(6)), vec![int(4)])), &[2]), of_t(held(2), &[2]));
    // The same through a correlated EXISTS on the key.
    let same_row = exists(one_from(0, and(vec![eq(int(4), int(0)), held(6)])));
    proves(of_t(same_row, &[2]), of_t(held(2), &[2]));
    // On a column that is no key, another row may carry the value.
    let by_a = of_t(in_(vec![int(2)], project(filter(scan(0), held(5)), vec![int(6)])), &[2]);
    refutes(by_a, of_t(held(1), &[2]));
}

#[test]
fn a_correlated_scalar_subquery_is_not_a_join_to_a_table_without_a_key() {
    // `SELECT k, (SELECT b FROM w WHERE w.k = t.fk) FROM t` against `SELECT t.k, w.b FROM t
    // LEFT JOIN w ON w.k = t.fk`: with two matching rows the join returns two rows, where the
    // subquery is an error.
    let scalar_b = scalar(project(filter(scan(2), eq(int(4), int(1))), vec![int(5)]), "INTEGER");
    let with_scalar = project(scan(0), vec![int(0), scalar_b]);
    let joined = project(json!({ "join": { "kind": "LEFT", "left": scan(0), "right": scan(2), "condition": eq(int(4), int(1)) } }), vec![int(0), int(5)]);
    refutes(with_scalar, joined);
}

#[test]
fn a_correlated_subquery_two_levels_deep_reads_the_outermost_row() {
    // `WHERE EXISTS (SELECT 1 FROM u WHERE u.k = t.fk AND EXISTS (SELECT 1 FROM w WHERE w.b = t.a))`
    // against the inner EXISTS lifted beside the outer one: it reads only `t`.
    // Inside `u`'s subquery (columns 4, 5) a further subquery numbers its own from 6.
    let inner = || exists(one_from(2, eq(int(7), int(2))));
    let nested = of_t(exists(one_from(1, and(vec![eq(int(4), int(1)), inner()]))), &[0]);
    let lifted = of_t(and(vec![exists(one_from(1, eq(int(4), int(1)))), exists(one_from(2, eq(int(5), int(2))))]), &[0]);
    proves(nested, lifted);
}
