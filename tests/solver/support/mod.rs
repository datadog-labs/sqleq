// Unless explicitly stated otherwise all files in this repository are licensed under the
// Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/).
// Copyright 2026-Present Datadog, Inc.

//! Builders for `Input` JSON in the shapes the frontend emits, so a test reads close to the SQL
//! it stands for. Each file under `tests/` uses its own subset.

#![allow(dead_code)]

use std::collections::HashMap;

use serde_json::{json, Value};
use sqleq::solver::eval::{Db, Env};
use sqleq::solver::ir::{Input, Type};
use sqleq::solver::prove::{verify, Verdict};
use sqleq::solver::translate::{translate_input, Query, OUT_VAR_ID};
use sqleq::solver::uterm::UConst;

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

/// The values a column of `ty` holds in the random databases [`agrees`] builds.
fn domain(ty: Type) -> Vec<UConst> {
    match ty {
        Type::Varchar => vec![UConst::Null, UConst::Str("a".into()), UConst::Str("b".into())],
        Type::Boolean => vec![UConst::Null, UConst::Int(0), UConst::Int(1)],
        _ => vec![UConst::Null, UConst::Int(0), UConst::Int(1), UConst::Int(2)],
    }
}

/// Both sides return the same bag on random small databases that keep the schemas' NOT NULL
/// columns, keys and BOOLEAN domain: the multiplicity of every output tuple over the universe.
/// Panics, naming the tuple and the tables, on the first difference.
pub fn agrees(schemas: &[Value], a: &Value, b: &Value) {
    let input = Input::parse(&json!({ "schemas": schemas, "queries": [a, b] })).unwrap();
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
            let count = |q: &Query| {
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
