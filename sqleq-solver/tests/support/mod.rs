// Unless explicitly stated otherwise all files in this repository are licensed under the
// Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/).
// Copyright 2026-Present Datadog, Inc.

//! Builders for `Input` JSON in the shapes the frontend emits, so a test reads close to the SQL
//! it stands for. Each file under `tests/` uses its own subset.

#![allow(dead_code)]

use serde_json::{json, Value};
use sqleq_solver::prove::{verify, Verdict};

/// A table: name, column types, which columns are nullable, and its unique keys.
pub fn table(name: &str, types: &[&str], nullable: &[bool], key: &[&[usize]]) -> Value {
    json!({ "name": name, "types": types, "nullable": nullable, "key": key })
}

pub fn col(index: u32, ty: &str) -> Value {
    json!({ "column": index, "type": ty })
}

pub fn lit(value: &str, ty: &str) -> Value {
    json!({ "operator": value, "operand": [], "type": ty })
}

pub fn call(op: &str, ty: &str, operand: Vec<Value>) -> Value {
    json!({ "operator": op, "type": ty, "operand": operand })
}

pub fn cast(ty: &str, operand: Value) -> Value {
    call("CAST", ty, vec![operand])
}

pub fn scan(i: usize) -> Value {
    json!({ "scan": i })
}

pub fn project(source: Value, target: Vec<Value>) -> Value {
    json!({ "project": { "source": source, "target": target } })
}

pub fn filter(source: Value, condition: Value) -> Value {
    json!({ "filter": { "source": source, "condition": condition } })
}

/// `GROUP BY keys` with these aggregate calls; no keys is a scalar aggregate.
pub fn group(source: Value, keys: Vec<Value>, function: Vec<Value>) -> Value {
    json!({ "group": { "source": source, "keys": keys, "function": function } })
}

/// `SELECT DISTINCT`, as the frontend lowers it: a keys-only group over the projection.
pub fn distinct(source: Value, types: &[&str]) -> Value {
    let keys = types.iter().enumerate().map(|(i, t)| col(i as u32, t)).collect();
    group(source, keys, vec![])
}

pub fn agg(op: &str, ty: &str, operand: Vec<Value>) -> Value {
    json!({ "operator": op, "type": ty, "distinct": false, "ignoreNulls": true, "operand": operand })
}

pub fn scalar(query: Value, ty: &str) -> Value {
    json!({ "operator": "$SCALAR_QUERY", "type": ty, "operand": [], "query": query })
}

pub fn exists(query: Value) -> Value {
    json!({ "operator": "EXISTS", "type": "BOOLEAN", "operand": [], "query": query })
}

pub fn pred(op: &str, operand: Vec<Value>) -> Value {
    call(op, "BOOLEAN", operand)
}

pub fn pair(schemas: Vec<Value>, a: Value, b: Value) -> Verdict {
    verify(&json!({ "schemas": schemas, "queries": [a, b] }))
}

pub fn proved(v: &Verdict) -> bool {
    matches!(v, Verdict::Eq { .. })
}
