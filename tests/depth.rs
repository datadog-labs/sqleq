// Unless explicitly stated otherwise all files in this repository are licensed under the
// Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/).
// Copyright 2026-Present Datadog, Inc.

//! Long and deeply nested predicates.
//!
//! An `AND`/`OR` chain lowers to one n-ary node, so the IR is as shallow as the predicate is wide:
//! a prover reads its input with a nesting limit, and a generated `WHERE` with hundreds of terms
//! used to exceed it. The parser accepts nesting well past its default limit of 50.

use serde_json::Value;
use sqleq_frontend::{lower_with, CatalogSource};

const T: &str = r#"create table "t" ("id" INTEGER, "a" INTEGER, "s" VARCHAR, "k" VARCHAR, unique ("id"));"#;

fn lower(q0: &str, q1: &str) -> Value {
    lower_with(&format!("{T}\n{q0};\n{q1};"), CatalogSource::InferredSeeded)
        .unwrap_or_else(|e| panic!("expected Ok, got {e}"))
}

fn identical(q0: &str, q1: &str) -> bool {
    let v = lower(q0, q1);
    v["queries"][0] == v["queries"][1]
}

/// The first node anywhere in `v` whose `"operator"` is `op`.
fn find_op<'a>(v: &'a Value, op: &str) -> Option<&'a Value> {
    if v.get("operator").and_then(Value::as_str) == Some(op) {
        return Some(v);
    }
    match v {
        Value::Object(m) => m.values().find_map(|x| find_op(x, op)),
        Value::Array(a) => a.iter().find_map(|x| find_op(x, op)),
        _ => None,
    }
}

fn operators_of(node: &Value) -> Vec<&str> {
    node["operand"].as_array().expect("operands").iter().filter_map(|o| o["operator"].as_str()).collect()
}

/// Runs `f` on a thread with room for an unoptimised build's frames. The parser's AST derives
/// `Clone`, `Drop` and its visitors, and each of them recurses once per term of a chain; a release
/// build on a main thread's stack lowers chains far longer than the ones below.
fn with_stack(f: impl FnOnce() + Send + 'static) {
    std::thread::Builder::new()
        .stack_size(64 << 20)
        .spawn(f)
        .expect("spawns")
        .join()
        .unwrap_or_else(|e| std::panic::resume_unwind(e));
}

/// JSON nesting depth, counted the way a JSON reader's recursion limit counts it.
fn depth(v: &Value) -> usize {
    match v {
        Value::Object(m) => 1 + m.values().map(depth).max().unwrap_or(0),
        Value::Array(a) => 1 + a.iter().map(depth).max().unwrap_or(0),
        _ => 0,
    }
}

#[test]
fn a_chain_lowers_like_its_regroupings() {
    let flat = "SELECT id FROM t WHERE a = 1 AND s = 'x' AND k = 'y' AND id = 2";
    assert!(identical(flat, "SELECT id FROM t WHERE (a = 1 AND s = 'x') AND (k = 'y' AND id = 2)"));
    assert!(identical(flat, "SELECT id FROM t WHERE a = 1 AND (s = 'x' AND (k = 'y' AND id = 2))"));
    let v = lower(flat, flat);
    assert_eq!(find_op(&v["queries"][0], "AND").expect("an AND")["operand"].as_array().map(Vec::len), Some(4));
    // The same in a projection, and in the post-aggregation scope.
    assert!(identical(
        "SELECT a = 1 OR s = 'x' OR k = 'y' FROM t",
        "SELECT a = 1 OR (s = 'x' OR k = 'y') FROM t"
    ));
    assert!(identical(
        "SELECT a FROM t GROUP BY a HAVING count(*) > 1 AND min(s) = 'x' AND max(k) = 'y'",
        "SELECT a FROM t GROUP BY a HAVING count(*) > 1 AND (min(s) = 'x' AND max(k) = 'y')"
    ));
}

#[test]
fn an_in_list_lowers_like_its_or_chain() {
    assert!(identical(
        "SELECT id FROM t WHERE k IN ($1, $2, $3)",
        "SELECT id FROM t WHERE k = $1 OR k = $2 OR k = $3"
    ));
    assert!(identical(
        "SELECT id FROM t WHERE k = ANY(ARRAY[$1, $2, $3])",
        "SELECT id FROM t WHERE k = $1 OR k = $2 OR k = $3"
    ));
}

#[test]
fn the_two_operators_are_not_merged() {
    let q = "SELECT id FROM t WHERE a = 1 AND (s = 'x' OR k = 'y') AND id = 2";
    let v = lower(q, q);
    let and = find_op(&v["queries"][0], "AND").expect("an AND");
    assert_eq!(operators_of(and), ["=", "OR", "="], "{and}");
    assert!(!identical(q, "SELECT id FROM t WHERE a = 1 AND s = 'x' OR k = 'y' AND id = 2"));
}

#[test]
fn a_negated_chain_is_not_distributed() {
    let q = "SELECT id FROM t WHERE a = 1 AND NOT (s = 'x' AND k = 'y')";
    let v = lower(q, q);
    let and = find_op(&v["queries"][0], "AND").expect("an AND");
    assert_eq!(operators_of(and), ["=", "NOT"], "{and}");
    assert!(!identical(q, "SELECT id FROM t WHERE a = 1 AND NOT s = 'x' AND NOT k = 'y'"));
}

#[test]
fn a_group_key_inside_a_chain_still_matches() {
    // `a = 1 AND s = 'x'` is a key; the chain around it reads the key's value, not its columns.
    let q = "SELECT (a = 1 AND s = 'x') AND k = 'y' FROM t GROUP BY a = 1 AND s = 'x', k";
    lower(q, q);
}

#[test]
fn a_thousand_term_chain_lowers_shallow() {
    with_stack(|| {
        let terms: Vec<String> = (0..1000).map(|i| format!("a = {i}")).collect();
        let q = format!("SELECT id FROM t WHERE {}", terms.join(" OR "));
        let v = lower(&q, &q);
        let or = find_op(&v["queries"][0], "OR").expect("an OR");
        assert_eq!(or["operand"].as_array().map(Vec::len), Some(1000));
        assert!(depth(&v["queries"][0]) < 20, "depth {}", depth(&v["queries"][0]));
    });
}

#[test]
fn a_deeply_parenthesised_chain_parses() {
    // `((((a = 0 AND a = 1) AND a = 2) ...)`: 200 levels, four times the parser's default limit.
    with_stack(|| {
        let mut p = "a = 0".to_string();
        for i in 1..=200 {
            p = format!("({p} AND a = {i})");
        }
        let q = format!("SELECT id FROM t WHERE {p}");
        let v = lower(&q, &q);
        let and = find_op(&v["queries"][0], "AND").expect("an AND");
        assert_eq!(and["operand"].as_array().map(Vec::len), Some(201));
    });
}
