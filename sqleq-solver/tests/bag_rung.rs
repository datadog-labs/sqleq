// Unless explicitly stated otherwise all files in this repository are licensed under the
// Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/).
// Copyright 2026-Present Datadog, Inc.

//! The bag congruence rung (`bagsolver`): pairs that say the same thing under bag semantics in two
//! ways, each proved pair also evaluated on random databases that keep the schema, and the pairs
//! that differ in Postgres, which it must not prove.

mod support;

use std::collections::HashMap;

use serde_json::{json, Value};
use sqleq_solver::eval::{Db, Env};
use sqleq_solver::ir::{Input, Type};
use sqleq_solver::translate::{translate_input, OUT_VAR_ID};
use sqleq_solver::uterm::UConst;
use support::*;

/// `t(k INTEGER NOT NULL, a INTEGER, v VARCHAR, flag BOOLEAN NOT NULL, opt BOOLEAN)`, key `(k)`;
/// `u(k INTEGER, w INTEGER)`, no key.
fn schemas() -> Vec<Value> {
    vec![
        table("t", &["INTEGER", "INTEGER", "VARCHAR", "BOOLEAN", "BOOLEAN"], &[false, true, true, false, true], &[&[0]]),
        table("u", &["INTEGER", "INTEGER"], &[true, true], &[]),
    ]
}

fn int(i: u32) -> Value {
    col(i, "INTEGER")
}

fn param(n: u32, ty: &str) -> Value {
    call(&format!("QP{n}"), ty, vec![lit("0", "INTEGER")])
}

fn eq(a: Value, b: Value) -> Value {
    pred("=", vec![a, b])
}

fn or(parts: Vec<Value>) -> Value {
    pred("OR", parts)
}

fn and(parts: Vec<Value>) -> Value {
    pred("AND", parts)
}

/// `SELECT k FROM t WHERE cond`.
fn ks(cond: Value) -> Value {
    project(filter(scan(0), cond), vec![int(0)])
}

/// The values a column of `ty` holds in the random databases below.
fn domain(ty: Type) -> Vec<UConst> {
    match ty {
        Type::Varchar => vec![UConst::Null, UConst::Str("a".into()), UConst::Str("b".into())],
        Type::Boolean => vec![UConst::Null, UConst::Int(0), UConst::Int(1)],
        _ => vec![UConst::Null, UConst::Int(0), UConst::Int(1), UConst::Int(2)],
    }
}

/// Both sides return the same bag on random small databases that keep the schema's NOT NULL
/// columns, keys and BOOLEAN domain: the multiplicity of every output tuple over the universe.
fn agrees(a: &Value, b: &Value) {
    let input = Input::parse(&json!({ "schemas": schemas(), "queries": [a, b] })).unwrap();
    let [qa, qb] = translate_input(&input).unwrap();
    let mut universe: Vec<UConst> = vec![UConst::Null, UConst::Int(0), UConst::Int(1), UConst::Int(2)];
    universe.extend([UConst::Str("a".into()), UConst::Str("b".into())]);
    let mut seed: u64 = 11;
    let mut next = |m: usize| {
        seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        (seed >> 33) as usize % m
    };
    for _ in 0..25 {
        let mut tables: HashMap<String, Vec<Vec<UConst>>> = HashMap::new();
        for s in &input.schemas {
            let mut rows: Vec<Vec<UConst>> = Vec::new();
            for _ in 0..next(4) {
                let row: Vec<UConst> = s
                    .types
                    .iter()
                    .enumerate()
                    .map(|(c, ty)| {
                        let d: Vec<UConst> = domain(*ty).into_iter().filter(|v| s.nullable[c] || *v != UConst::Null).collect();
                        d[next(d.len())].clone()
                    })
                    .collect();
                let clash = s.key.iter().any(|k| rows.iter().any(|r| k.iter().all(|c| r[*c] == row[*c])));
                if !clash {
                    rows.push(row);
                }
            }
            tables.insert(s.name.clone(), rows);
        }
        let arity = qa.arity;
        let mut tuple = vec![0usize; arity];
        loop {
            let out: Vec<UConst> = tuple.iter().map(|i| universe[*i].clone()).collect();
            let count = |q: &sqleq_solver::translate::Query| {
                let db = Db::new(tables.clone(), universe.clone(), q.widths.clone());
                db.count(&q.term, &mut Env::from([(OUT_VAR_ID, out.clone())])).unwrap()
            };
            assert_eq!(count(&qa), count(&qb), "{out:?} on {tables:?}");
            let Some(i) = (0..arity).find(|i| tuple[*i] + 1 < universe.len()) else { break };
            tuple[..i].iter_mut().for_each(|x| *x = 0);
            tuple[i] += 1;
        }
    }
}

fn proves(a: Value, b: Value) {
    assert!(proved(&pair(schemas(), a.clone(), b.clone())), "not proved:\n{a}\n{b}");
    agrees(&a, &b);
}

fn refutes(a: Value, b: Value) {
    assert!(!proved(&pair(schemas(), a.clone(), b.clone())), "proved:\n{a}\n{b}");
}

/// `UPDATE t SET v = $1 WHERE k = $2 [AND guard]`, lowered as the table's new contents.
fn update_v(guard: Option<Value>) -> Value {
    let hit = eq(int(0), param(2, "INTEGER"));
    let when = match guard {
        Some(g) => and(vec![hit, g]),
        None => hit,
    };
    let v = call("CASE", "VARCHAR", vec![when, param(1, "VARCHAR"), col(2, "VARCHAR")]);
    project(scan(0), vec![int(0), int(1), v, col(3, "BOOLEAN"), col(4, "BOOLEAN")])
}

#[test]
fn an_update_that_skips_rows_already_holding_the_value_is_the_same_update() {
    let distinct = pred("IS DISTINCT FROM", vec![col(2, "VARCHAR"), param(1, "VARCHAR")]);
    proves(update_v(None), update_v(Some(distinct)));
    // `<>` is UNKNOWN on a NULL `v`, so that row keeps NULL where the first sets `$1`.
    let ne = pred("<>", vec![col(2, "VARCHAR"), param(1, "VARCHAR")]);
    refutes(update_v(None), update_v(Some(ne)));
}

#[test]
fn the_random_databases_tell_a_differing_pair_apart() {
    // Otherwise `agrees` would check nothing.
    let ne = pred("<>", vec![col(2, "VARCHAR"), param(1, "VARCHAR")]);
    let caught = std::panic::catch_unwind(|| agrees(&update_v(None), &update_v(Some(ne))));
    assert!(caught.is_err(), "the random databases never told the `<>` update apart");
}

#[test]
fn an_in_list_the_disjunction_implies_changes_nothing() {
    // `SELECT a, count(a) FROM t WHERE (a = $1 AND k < $2) OR a = $3 GROUP BY a`, and the same with
    // `a IN ($1, $3) AND` in front.
    let disjunction = || or(vec![and(vec![eq(int(1), param(1, "INTEGER")), pred("<", vec![int(0), param(2, "INTEGER")])]), eq(int(1), param(3, "INTEGER"))]);
    let in_list = or(vec![eq(int(1), param(1, "INTEGER")), eq(int(1), param(3, "INTEGER"))]);
    let counted = |cond| group(project(filter(scan(0), cond), vec![int(1)]), vec![int(0)], vec![agg("COUNT", "INTEGER", vec![int(0)])]);
    proves(counted(disjunction()), counted(and(vec![in_list, disjunction()])));
}

#[test]
fn a_repeated_disjunct_is_one_disjunct() {
    let is_null = || pred("IS NULL", vec![col(2, "VARCHAR")]);
    proves(ks(or(vec![is_null(), is_null()])), ks(is_null()));
}

#[test]
fn a_boolean_is_true_false_or_null() {
    let count = |cond| group(project(filter(scan(0), cond), vec![int(0)]), vec![], vec![agg("COUNT", "INTEGER", vec![])]);
    let either = |c: u32| or(vec![eq(col(c, "BOOLEAN"), lit("TRUE", "BOOLEAN")), eq(col(c, "BOOLEAN"), lit("FALSE", "BOOLEAN"))]);
    let held = || eq(int(1), param(1, "INTEGER"));
    // `flag` is NOT NULL, so `flag IN (TRUE, FALSE)` holds on every row.
    proves(count(held()), count(and(vec![held(), either(3)])));
    // `opt` may be NULL, and then it does not.
    refutes(count(held()), count(and(vec![held(), either(4)])));
    // `opt IS NOT NULL` is `opt IS TRUE OR opt IS FALSE` (the frontend writes `x IS FALSE` as
    // `(NOT x) IS TRUE`).
    let is_true = |x: Value| pred("IS TRUE", vec![x]);
    let opt = || col(4, "BOOLEAN");
    proves(ks(pred("IS NOT NULL", vec![opt()])), ks(or(vec![is_true(opt()), is_true(pred("NOT", vec![opt()]))])));
}

#[test]
fn is_not_distinct_from_is_equality_or_both_null() {
    let a = || int(1);
    let p = || param(1, "INTEGER");
    let a_null = || pred("IS NULL", vec![a()]);
    proves(ks(or(vec![eq(a(), p()), a_null()])), ks(or(vec![pred("IS NOT DISTINCT FROM", vec![a(), p()]), a_null()])));
}

#[test]
fn what_differs_is_not_proved() {
    // `a IS NOT NULL OR a > 0` keeps a = -1-like rows that `a > 0` drops.
    let gt = || pred(">", vec![int(1), lit("0", "INTEGER")]);
    refutes(ks(or(vec![pred("IS NOT NULL", vec![int(1)]), gt()])), ks(gt()));
    // The sum of two CASEs is not one CASE: where both conditions hold it adds both values.
    let (p, q) = (|| eq(int(1), param(1, "INTEGER")), || eq(int(0), param(2, "INTEGER")));
    let zero = || lit("0", "INTEGER");
    let added = call("+", "INTEGER", vec![call("CASE", "INTEGER", vec![p(), int(1), zero()]), call("CASE", "INTEGER", vec![q(), int(0), zero()])]);
    let chosen = call("CASE", "INTEGER", vec![p(), int(1), q(), int(0), zero()]);
    refutes(project(scan(0), vec![added]), project(scan(0), vec![chosen]));
    // A join to a table without a key counts a row once per match; `IN` counts it once.
    let joined = project(json!({ "join": { "kind": "INNER", "left": scan(0), "right": scan(1), "condition": eq(int(0), int(5)) } }), vec![int(1)]);
    let member = project(filter(scan(0), json!({ "operator": "IN", "type": "BOOLEAN", "operand": [int(0)], "query": project(scan(1), vec![int(5)]) })), vec![int(1)]);
    refutes(joined, member);
}
