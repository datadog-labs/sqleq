// Unless explicitly stated otherwise all files in this repository are licensed under the
// Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/).
// Copyright 2026-Present Datadog, Inc.

//! The encoding end to end over hand-built IR, in the shapes the frontend emits: what it proves,
//! what it refutes with an exact model, what it answers under uninterpreted symbols, what it
//! refuses, and where the two semantics part.

use serde_json::{json, Value};
use sqleq_cvc5::{check, smt2, Failure, Options, Outcome, Refusal, Semantics};
use sqleq_solver::ir::{Input, TranslateError};

fn table(name: &str, types: &[&str], nullable: &[bool], key: &[&[usize]]) -> Value {
    json!({ "name": name, "types": types, "nullable": nullable, "key": key })
}
fn col(index: u32, ty: &str) -> Value {
    json!({ "column": index, "type": ty })
}
fn lit(value: &str, ty: &str) -> Value {
    json!({ "operator": value, "operand": [], "type": ty })
}
fn call(op: &str, ty: &str, operand: Vec<Value>) -> Value {
    json!({ "operator": op, "type": ty, "operand": operand })
}
fn pred(op: &str, operand: Vec<Value>) -> Value {
    call(op, "BOOLEAN", operand)
}
fn scan(i: usize) -> Value {
    json!({ "scan": i })
}
fn project(source: Value, target: Vec<Value>) -> Value {
    json!({ "project": { "source": source, "target": target } })
}
fn filter(source: Value, condition: Value) -> Value {
    json!({ "filter": { "source": source, "condition": condition } })
}
fn group(source: Value, keys: Vec<Value>, function: Vec<Value>) -> Value {
    json!({ "group": { "source": source, "keys": keys, "function": function } })
}
fn agg(op: &str, ty: &str, operand: Vec<Value>) -> Value {
    json!({ "operator": op, "type": ty, "distinct": false, "ignoreNulls": true, "operand": operand })
}
fn join(left: Value, right: Value, kind: &str, condition: Value) -> Value {
    json!({ "join": { "left": left, "right": right, "kind": kind, "condition": condition } })
}

fn opts(sem: Semantics) -> Options {
    Options { semantics: sem, timeout_ms: Some(20_000), ..Options::default() }
}

fn run(sem: Semantics, schemas: Vec<Value>, a: Value, b: Value) -> Result<Outcome, Failure> {
    let input = Input::parse(&json!({ "schemas": schemas, "queries": [a, b] })).expect("well-formed");
    check(&input, &opts(sem))
}

fn both(schemas: Vec<Value>, a: Value, b: Value) -> [Result<Outcome, Failure>; 2] {
    [run(Semantics::Bags, schemas.clone(), a.clone(), b.clone()), run(Semantics::Sets, schemas, a, b)]
}

/// `t(a INTEGER, b VARCHAR)`, both nullable, no key.
fn t() -> Vec<Value> {
    vec![table("t", &["INTEGER", "VARCHAR"], &[true, true], &[])]
}

#[test]
fn a_filter_dropped_is_refuted_with_an_exact_model() {
    let a_is_one = pred("=", vec![col(0, "INTEGER"), lit("1", "INTEGER")]);
    for r in both(t(), filter(scan(0), a_is_one), scan(0)) {
        match r {
            Ok(Outcome::Different { model, exact }) => {
                assert!(exact);
                assert_eq!(model.tables.len(), 1);
                assert_eq!(model.tables[0].name, "t");
                assert!(!model.tables[0].rows.is_empty(), "{}", model.summary());
            }
            other => panic!("{other:?}"),
        }
    }
}

#[test]
fn a_bare_order_by_is_a_proof_and_a_row_slice_a_refusal() {
    let sorted = json!({ "sort": { "source": scan(0), "collation": [[0, "INTEGER", "ASCENDING NULLS LAST"]] } });
    for r in both(t(), scan(0), sorted) {
        assert_eq!(r, Ok(Outcome::Equivalent));
    }
    let limited = json!({ "sort": { "source": scan(0), "collation": [], "limit": lit("1", "INTEGER") } });
    for r in both(t(), scan(0), limited) {
        assert_eq!(r, Err(Failure::Refused(Refusal::Ir(TranslateError::UnsupportedSort))));
    }
}

#[test]
fn union_all_and_union_part_under_bags_and_meet_under_sets() {
    let a = project(scan(0), vec![col(0, "INTEGER")]);
    let all = json!({ "union": [a.clone(), a.clone()] });
    let distinct = json!({ "distinct": all.clone() });
    let [bags, sets] = both(t(), all, distinct);
    assert!(matches!(bags, Ok(Outcome::Different { exact: true, .. })), "{bags:?}");
    assert_eq!(sets, Ok(Outcome::Equivalent));
}

#[test]
fn a_key_makes_the_in_to_join_rewrite_a_proof_under_sets() {
    // users(id, team_id), teams(id PRIMARY KEY, plan): the README's quick start.
    let schemas = vec![
        table("users", &["INTEGER", "INTEGER"], &[true, true], &[]),
        table("teams", &["INTEGER", "INTEGER"], &[false, true], &[&[0]]),
    ];
    // Inside the subquery the columns start after the enclosing row's two: teams.id is 2, plan 3.
    let plan_two = filter(scan(1), pred("=", vec![col(3, "INTEGER"), lit("2", "INTEGER")]));
    let subquery = json!({ "operator": "IN", "type": "BOOLEAN", "operand": [col(1, "INTEGER")],
                           "query": project(plan_two, vec![col(2, "INTEGER")]) });
    let a = project(filter(scan(0), subquery), vec![col(0, "INTEGER")]);
    let on = pred("=", vec![col(2, "INTEGER"), col(1, "INTEGER")]);
    let joined = filter(join(scan(0), scan(1), "INNER", on), pred("=", vec![col(3, "INTEGER"), lit("2", "INTEGER")]));
    let b = project(joined, vec![col(0, "INTEGER")]);
    assert_eq!(run(Semantics::Sets, schemas.clone(), a.clone(), b.clone()), Ok(Outcome::Equivalent));
    // Without the key the rewrite is wrong under bag semantics -- two teams with one id repeat a
    // user -- and the model says so; under set semantics the repetition vanishes, and it holds.
    let keyless = vec![schemas[0].clone(), table("teams", &["INTEGER", "INTEGER"], &[false, true], &[])];
    match run(Semantics::Bags, keyless, a, b) {
        Ok(Outcome::Different { exact: true, .. }) | Ok(Outcome::Unknown(_)) => {}
        other => panic!("{other:?}"),
    }
}

#[test]
fn count_star_over_a_group_is_proved_against_a_renaming_and_refuted_against_count_of_a_nullable() {
    let star = group(scan(0), vec![col(1, "VARCHAR")], vec![agg("COUNT", "INTEGER", vec![])]);
    let of_a = group(scan(0), vec![col(1, "VARCHAR")], vec![agg("COUNT", "INTEGER", vec![col(0, "INTEGER")])]);
    // The same aggregate over an ordered source: one term, since the order is erased.
    let sorted = json!({ "sort": { "source": scan(0), "collation": [[0, "INTEGER", "ASCENDING NULLS LAST"]] } });
    let through = group(sorted, vec![col(1, "VARCHAR")], vec![agg("COUNT", "INTEGER", vec![])]);
    assert_eq!(run(Semantics::Bags, t(), star.clone(), through), Ok(Outcome::Equivalent));
    // COUNT(*) against COUNT(a) differ on a row with a null `a`: a model says so when the bags
    // solver finds one within the budget; the fold reasoning behind `table.aggr` may not.
    match run(Semantics::Bags, t(), star, of_a) {
        Ok(Outcome::Different { model, exact: true }) => {
            assert!(model.tables[0].rows.iter().any(|(cells, _)| cells[0].is_none()), "{}", model.summary());
        }
        Ok(Outcome::Unknown(_)) => {}
        other => panic!("{other:?}"),
    }
}

#[test]
fn an_uninterpreted_function_proves_but_only_offers_a_candidate_model() {
    let f = |c: Value| call("q_conv_integer_varchar", "VARCHAR", vec![c]);
    let a = project(scan(0), vec![f(col(0, "INTEGER"))]);
    // The same call over `5 + 0` and over `5`: a proof through the uninterpreted symbol, once the
    // argument is rewritten.
    let five_plus_zero = call("+", "INTEGER", vec![lit("5", "INTEGER"), lit("0", "INTEGER")]);
    let a5 = project(scan(0), vec![f(lit("5", "INTEGER"))]);
    assert_eq!(run(Semantics::Bags, t(), a5, project(scan(0), vec![f(five_plus_zero)])), Ok(Outcome::Equivalent));
    let b = project(scan(0), vec![col(1, "VARCHAR")]);
    match run(Semantics::Bags, t(), a, b) {
        Ok(Outcome::Different { exact: false, .. }) => {}
        other => panic!("{other:?}"),
    }
}

#[test]
fn integer_division_truncates_toward_zero() {
    // `a + 10 / 2` against `a + 5`, with the division as the frontend names it.
    let div = call("q_arith_div_integer_integer", "INTEGER", vec![lit("10", "INTEGER"), lit("2", "INTEGER")]);
    let a = project(scan(0), vec![call("+", "INTEGER", vec![col(0, "INTEGER"), div])]);
    let b = project(scan(0), vec![call("+", "INTEGER", vec![col(0, "INTEGER"), lit("5", "INTEGER")])]);
    assert_eq!(run(Semantics::Bags, t(), a, b), Ok(Outcome::Equivalent));
    // `-7 / 2` is -3 in Postgres, not -4: the pair against `-4` has a model, against `-3` none.
    let q = |n: &str| {
        let d = call("/", "INTEGER", vec![lit("-7", "INTEGER"), lit("2", "INTEGER")]);
        (project(scan(0), vec![d]), project(scan(0), vec![lit(n, "INTEGER")]))
    };
    let (d, minus3) = q("-3");
    assert_eq!(run(Semantics::Bags, t(), d, minus3), Ok(Outcome::Equivalent));
    let (d, minus4) = q("-4");
    assert!(matches!(run(Semantics::Bags, t(), d, minus4), Ok(Outcome::Different { exact: true, .. })));
}

#[test]
fn rows_of_different_shapes_are_incomparable_not_refuted() {
    let one = project(scan(0), vec![col(0, "INTEGER")]);
    let two = project(scan(0), vec![col(0, "INTEGER"), col(1, "VARCHAR")]);
    match run(Semantics::Bags, t(), one.clone(), two) {
        Ok(Outcome::Incomparable(note)) => assert!(note.contains("1 and 2 columns"), "{note}"),
        other => panic!("{other:?}"),
    }
    // An integer column against a real one is compared numerically, so the pair is checked rather
    // than left incomparable; whether cvc5 then proves it is its own affair.
    let sum_int = group(scan(0), vec![], vec![agg("SUM", "INTEGER", vec![col(0, "INTEGER")])]);
    let sum_real = group(scan(0), vec![], vec![agg("SUM", "REAL", vec![col(0, "INTEGER")])]);
    match run(Semantics::Bags, t(), sum_int, sum_real) {
        Ok(Outcome::Equivalent) | Ok(Outcome::Unknown(_)) => {}
        other => panic!("{other:?}"),
    }
    let _ = one;
}

#[test]
fn a_correlated_subquery_is_encoded() {
    // the inner query reads the outer row's column 0 as column 2 of its scope: encoded, its filter
    // reading the enclosing lambda's variable, and not refused
    let inner = filter(scan(0), pred("=", vec![col(2, "INTEGER"), col(0, "INTEGER")]));
    let exists = json!({ "operator": "EXISTS", "type": "BOOLEAN", "operand": [], "query": inner });
    for r in both(t(), filter(scan(0), exists), scan(0)) {
        assert!(!matches!(r, Err(Failure::Refused(_))), "{r:?}");
    }
}

#[test]
fn the_smt2_text_names_the_table_and_the_option() {
    let input = Input::parse(&json!({ "schemas": t(), "queries": [scan(0), scan(0)] })).unwrap();
    let text = smt2(&input, &Options { cvc5_options: vec![("bags-to-liastar".into(), "true".into())], ..opts(Semantics::Bags) }).unwrap();
    assert!(text.contains("(set-option :bags-to-liastar true)"), "{text}");
    assert!(text.contains("(declare-const t (Bag (Tuple (Nullable Int) (Nullable String))))"), "{text}");
    assert!(text.trim_end().ends_with("(get-model)"), "{text}");
}
