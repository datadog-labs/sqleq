// Unless explicitly stated otherwise all files in this repository are licensed under the
// Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/).
// Copyright 2026-Present Datadog, Inc.

//! An inferred parameter is never narrower than the one Postgres binds.
//!
//! A client that sends `$N` without a type leaves it to Postgres, which types it at its **first
//! use**, in the order it analyses the statement, from the operand it meets there: in
//! `n + 0 = $1 AND $1 > 0`, over a `numeric` `n`, `$1` is a `numeric`, and `$1 > 0` then compares two
//! numerics. Type inference ([`crate::infer`]) does not follow that order: it ranks its evidence by
//! confidence, and it reads no type off an expression, an aggregate or a subquery. So there it
//! types `$1` from the literal, an integer, and a prover that quantifies `qp1` over the integers
//! finds no value strictly between `0` and `1`: QED proved `n + 0 = $1 AND $1 > 0 AND $1 < 1`
//! equivalent to `false` (issue #122).
//!
//! An inferred type that is wider than Postgres's is harmless: `REAL` where Postgres says integer
//! quantifies over more values, and the reads that tell a `numeric` from an integer, a division or a
//! cast to text, are [`crate::equality`]'s. A narrower one is not. So a lowered query in which an
//! inferred INTEGER parameter is an operand of an operation that gives all its operands one type
//! (a comparison, arithmetic, `CASE`, `coalesce` and its kin) next to a `numeric` or float operand,
//! or is cast to one, is refused: if that operand is the parameter's first use, Postgres typed it as
//! a `numeric` or a float. Where an integer use comes first, the integer is right, but the order is
//! not one the frontend follows, so it refuses then too: `$1 < 1 AND n * 2 = $1` is the price.
//!
//! Only parameters whose type inference gave are checked. A `declare` line that types `QP<n>` stands
//! for a client that declared the parameter's type, which Postgres takes as given.

use std::collections::BTreeSet;

use serde_json::Value;

use crate::error::{unsupported, Result};
use crate::types::{coarse_class, ty_of};

/// The operations whose operands Postgres resolves to one type, so that an untyped parameter among
/// them takes the type of the others: the comparisons, the arithmetic operators (integer division
/// and modulo are their own functions), `CASE`, and the calls that take their arguments' common type.
const ONE_TYPE: &[&str] = &[
    "=", "<>", "!=", "<", "<=", ">", ">=", "+", "-", "*", "/", "%", "CASE", "COALESCE", "NULLIF",
    "GREATEST", "LEAST",
];

/// Refuse a lowered pair in which a parameter typed by inference as an integer meets a `numeric` or
/// float value it would take its type from at its first use. `inferred` names the `QP<n>` functions
/// whose type inference gave.
pub fn refuse_narrowed(input: &Value, inferred: &BTreeSet<String>) -> Result<()> {
    if inferred.is_empty() {
        return Ok(());
    }
    for q in input["queries"].as_array().into_iter().flatten() {
        walk(q, inferred)?;
    }
    Ok(())
}

fn walk(v: &Value, inferred: &BTreeSet<String>) -> Result<()> {
    match v {
        Value::Object(m) => {
            if let Some(operand) = m.get("operand").and_then(Value::as_array) {
                let op = m.get("operator").and_then(Value::as_str).unwrap_or("");
                if ONE_TYPE.contains(&op) || op.starts_with("q_arith_") {
                    if let Some(p) = operand.iter().find_map(|o| narrow_param(o, inferred)) {
                        if let Some(class) = operand.iter().find_map(wide_class) {
                            return Err(refusal(&p, class));
                        }
                    }
                } else if op == "CAST" {
                    if let Some(p) = operand.first().and_then(|o| narrow_param(o, inferred)) {
                        if let Some(class) = wide_class(v) {
                            return Err(refusal(&p, class));
                        }
                    }
                }
            }
            m.values().try_for_each(|x| walk(x, inferred))
        }
        Value::Array(a) => a.iter().try_for_each(|x| walk(x, inferred)),
        _ => Ok(()),
    }
}

/// The `$N` a node stands for, if it is a parameter inference typed as an integer.
fn narrow_param(v: &Value, inferred: &BTreeSet<String>) -> Option<String> {
    let name = v.get("operator")?.as_str()?;
    let n = name.strip_prefix("QP").filter(|d| !d.is_empty() && d.bytes().all(|b| b.is_ascii_digit()))?;
    (inferred.contains(name) && ty_of(v) == "INTEGER").then(|| format!("${n}"))
}

/// The class of a value of a `numeric` or float type: one a parameter would take at its first use.
fn wide_class(v: &Value) -> Option<&'static str> {
    match coarse_class(&ty_of(v)) {
        Some("numeric") => Some("numeric"),
        Some("float") => Some("float"),
        _ => None,
    }
}

fn refusal(param: &str, class: &str) -> crate::error::FrontendError {
    unsupported(format!(
        "{param}, inferred as an integer, meets a {class} value: Postgres types an untyped parameter \
         at its first use, which may be this one, as a {class}"
    ))
}
