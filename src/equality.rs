// Unless explicitly stated otherwise all files in this repository are licensed under the
// Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/).
// Copyright 2026-Present Datadog, Inc.

//! Values that `=` calls equal and that are still two values.
//!
//! Two queries are equivalent when they return the same bag of rows, where two rows are the same if
//! their values are equal under SQL `=`. `2.0` and `2.00` are one value in that comparison, though
//! Postgres prints them differently, and so are `-0` and `0`. That is the reading every axis takes,
//! and the one the IR's provers can take: both read the IR's `=` as identity and deduplicate by it,
//! so a value in the IR stands for a class of Postgres values under `=`.
//!
//! For most types the class has one member. For `numeric`, the floats, `interval` and `jsonb` it has
//! more: `2.0 = 2.00`, `-0 = 0`, `'1 day' = '24 hours'` (and `'1 mon' = '30 days'`), and
//! `'{"a": 1.0}' = '{"a": 1.00}'`, and an array of one of them compares its elements the same way.
//! Reading such a value as its class is faithful while every operation on it gives equal results on
//! equal arguments. It is not faithful for one that does not: a cast to text, `||`, `concat`,
//! `scale`, `->>`, numeric division (whose scale follows its operands', so `1.0 / 3` and
//! `1.000000000000000000000 / 3` differ), `avg` over `numeric` (a division), or `date + interval`
//! (`'1 mon'` and `'30 days'` add differently). A prover reads each as a function of the class, so
//! from `t.x = u.x` it concludes `f(t.x) = f(u.x)`, and Postgres does not honour that. The equality
//! need not be written: a `GROUP BY`, a `DISTINCT` or a `UNION` puts equal values in one class too.
//!
//! So [`refuse_observed`] walks each lowered query and sorts every read of such a value:
//!
//! * by an operation known to give equal results on equal arguments ([`reads`]): a comparison,
//!   `count`, `min`, `max`, `sum`, `+`, `-`, `*`, `%`, float arithmetic, a cast to a number or a
//!   boolean, `coalesce`, `abs`, `round`, `->`, `@>`, a subscript and the like. Nothing to do. Where
//!   the result is a value of the same kind (`coalesce(x, 0)`), its type says so: the lowering names
//!   it with [`COARSE_OPAQUE`] where it would otherwise be VARBINARY, so a read of it further up, in
//!   another query block included, is sorted the same way.
//! * by any other operation. If the value is *fixed by its spelling* -- computed by one expression
//!   from literals, parameters, clocks and values of types whose `=` is identity -- the operation
//!   reads it through `q_exact_<type>(<that expression>, <those values>)`. Postgres evaluates one
//!   expression on equal inputs to one value, printed one way, and a prover can equate two
//!   `q_exact` terms only where the expressions are spelled alike and the inputs are equal, so the
//!   function it reads is the real one. That keeps `ts + INTERVAL '1 day'`, `n / 2.0` over an
//!   integer `n` and `CAST(2.0 AS TEXT)`. Otherwise the pair is refused.
//!
//! Except where the two queries lowered to one plan, which [`crate::types::refuse_unfaithful`]
//! explains for citext: one plan computes one thing however `=` is read.
//!
//! What this does not see is a value whose type the frontend does not know: the result of a function
//! nobody declared is VARBINARY, which is identity here, though `sqrt(i)` is a float. Opaque types
//! whose `=` is not identity beyond these four -- `numrange`, the geometric types, a domain over
//! `numeric` -- are read as identity too.

use serde_json::{json, Value};

use crate::error::{unsupported, FrontendError, Result};
use crate::types::{coarse_class, name_part, rename_emitted_types, ty_of, COARSE_OPAQUE};

/// The functions whose result is computed from their arguments' values, or is one of their
/// arguments: `coalesce`, `nullif`, `greatest` and `least` return an argument, `abs`, `ceil`,
/// `floor`, `round`, `trunc` and `sign` round or take the sign of a number by its value
/// (`round(2.0, 1)` and `round(2.00, 1)` are both `2.0`), and `->`, `#>` and `jsonb_extract_path`
/// take a part of a `jsonb`, which compares its parts as it compares the whole. Their result is a
/// value of the same kind, so the lowering gives it its argument's class ([`call_type`]).
///
/// Matched on the name the IR carries: unqualified and upper-cased, as the lowering writes a call,
/// and the demoted JSON operators as `normalize::demote_operators` names them.
pub const PASS_THROUGH: [&str; 13] = [
    "COALESCE",
    "NULLIF",
    "GREATEST",
    "LEAST",
    "ABS",
    "CEIL",
    "FLOOR",
    "ROUND",
    "TRUNC",
    "SIGN",
    "Q_OP_JSONX",
    "Q_OP_JSONPATH",
    "Q_OP_JSONPATH_ELEMS",
];

/// The type of a call to `name` over `operand`, given the type `ret` it would otherwise have: a
/// [`PASS_THROUGH`] function nobody declared returns VARBINARY, which would hide that
/// `coalesce(x, 0)` over a `numeric` `x` is a `numeric`, so it gets `x`'s class as a
/// [`COARSE_OPAQUE`] name instead. VARBINARY to the provers either way.
pub fn call_type(name: &str, operand: &[Value], ret: String) -> String {
    if ret == "VARBINARY" && PASS_THROUGH.contains(&name) {
        if let Some(class) = operand.iter().find_map(|v| coarse_class(&ty_of(v)).map(str::to_string)) {
            return format!("{COARSE_OPAQUE}{class}");
        }
    }
    ret
}

/// Refuse a lowered pair one of whose operations reads a value of a type whose `=` is not identity
/// and can tell two values apart that `=` calls equal; read such a value through `q_exact_<type>`
/// where its spelling fixes it. See the module docs. Mutates `input`, and only when the two queries
/// lowered to two plans.
pub fn refuse_observed(input: &mut Value) -> Result<()> {
    if input["queries"][0] == input["queries"][1] {
        return Ok(());
    }
    if let Some(queries) = input.get_mut("queries").and_then(Value::as_array_mut) {
        for q in queries {
            walk(q)?;
        }
    }
    Ok(())
}

/// How an operation reads a value of a type whose `=` is not identity.
#[derive(Debug, PartialEq, Eq)]
enum Read {
    /// Equal arguments give equal results, of a type whose `=` is identity: a comparison, `count`,
    /// a cast to an integer.
    Value,
    /// Equal arguments give equal results, of the same kind: `coalesce`, `+`, `round`. The result's
    /// own type must say so, or a read of it further up would take it for identity.
    Through,
    /// Not known to give equal results on equal arguments.
    Observes,
}

/// How the operation `op`, of result type `ty` and with operands of types `types`, reads an operand
/// whose type's `=` is not identity.
fn reads(op: &str, ty: &str, types: &[String]) -> Read {
    match op {
        // A comparison is a function of the classes by definition, and so is a null test.
        // `count` and `regr_count` count, and `min` and `max` order by the same comparison. `@>` and
        // `&&` compare the elements of an array, or the parts of a `jsonb`, with their own `=`.
        "=" | "<>" | "<" | ">" | "<=" | ">=" | "IS DISTINCT FROM" | "IS NOT DISTINCT FROM" | "IS NULL"
        | "IS NOT NULL" | "IN" | "COUNT" | "REGR_COUNT" | "MIN" | "MAX" | "Q_BOOL_CONTAINS" | "Q_BOOL_OVERLAP" => {
            Read::Value
        }
        _ if quantified(op) || op.starts_with("q_row_eq_") || op.starts_with("DISTINCT_ON#") => Read::Value,
        // `numeric` `+`, `-`, `*` and `%` are exact, so their result's value is a function of their
        // operands' values, and so is a sum's. Over an interval, `sum` adds months, days and
        // microseconds apart, and `=` compares a linear combination of the three.
        "SUM" | "CASE" | "+" | "-" | "*" | "%" => Read::Through,
        _ if PASS_THROUGH.contains(&op) || op.starts_with("q_subscript_") || op.starts_with("q_array_") => {
            Read::Through
        }
        // Float arithmetic: `-0` and `0` give `=`-equal results under `+`, `-`, `*` and `/` (a zero
        // divisor raises an error either way), and so does every other float, `NaN` included.
        _ if op.starts_with("q_arith_") && float_arithmetic(op, types) => Read::Through,
        // A cast between number types, or to a boolean, converts by value: `2.0::int` and
        // `2.00::int` are both 2, `'-0'::float8::numeric` is 0. A cast to anything else -- text
        // above all -- is not known to.
        _ if is_cast(op) => match ty {
            "INTEGER" | "BOOLEAN" => Read::Value,
            t if coarse_class(t).is_some() => Read::Through,
            _ => Read::Observes,
        },
        _ => Read::Observes,
    }
}

/// `= ANY`, `<> ALL` and the other quantified comparisons, as the lowering spells them.
fn quantified(op: &str) -> bool {
    op.split_once(' ').is_some_and(|(cmp, q)| {
        matches!(cmp, "=" | "<>" | "<" | ">" | "<=" | ">=") && matches!(q, "ANY" | "ALL" | "SOME")
    })
}

/// A `CAST`, a named `q_cast_` for a qualified or opaque target such as `numeric(10,2)` or `float8`,
/// or a `qcastK` of the inferring modes.
fn is_cast(op: &str) -> bool {
    // `get`, not slicing: an operator is any function's name, and a name need not be ASCII.
    let qcast = op.get(..5).is_some_and(|p| p.eq_ignore_ascii_case("qcast"))
        && op.get(5..).is_some_and(|k| !k.is_empty() && k.bytes().all(|b| b.is_ascii_digit()));
    op == "CAST" || op.starts_with("q_cast_") || qcast
}

/// `+`, `-`, `*` or `/` over numbers, one of them a float: Postgres converts the other to a float by
/// its value and computes in floating point.
fn float_arithmetic(op: &str, types: &[String]) -> bool {
    let arith = ["q_arith_add_", "q_arith_sub_", "q_arith_mul_", "q_arith_div_"].iter().any(|p| op.starts_with(p));
    let float = |t: &String| coarse_class(t) == Some("float");
    arith
        && types.iter().any(float)
        && types.iter().all(|t| float(t) || matches!(t.as_str(), "INTEGER" | "REAL"))
}

/// The class of `v`'s type, if its `=` is not identity and `v` is not already exact: a NULL has no
/// spelling to tell apart, and a `q_exact_` term is read by its spelling already.
fn coarse(v: &Value) -> Option<String> {
    let op = v.get("operator").and_then(Value::as_str).unwrap_or("");
    let nullary = v.get("operand").and_then(Value::as_array).is_some_and(|a| a.is_empty());
    if (op == "NULL" && nullary) || op.starts_with("q_exact_") {
        return None;
    }
    coarse_class(&ty_of(v)).map(str::to_string)
}

fn walk(v: &mut Value) -> Result<()> {
    match v {
        Value::Object(m) => {
            let node = match (m.get("operator"), m.get("operand")) {
                (Some(Value::String(op)), Some(Value::Array(operands))) => {
                    let ty = m.get("type").and_then(Value::as_str).unwrap_or("INTEGER").to_string();
                    let types: Vec<String> = operands.iter().map(ty_of).collect();
                    Some((op.clone(), ty, types))
                }
                _ => None,
            };
            if let Some((op, ty, types)) = node {
                let read = reads(&op, &ty, &types);
                if let Some(Value::Array(operands)) = m.get_mut("operand") {
                    for operand in operands.iter_mut() {
                        let Some(class) = coarse(operand) else { continue };
                        match read {
                            Read::Value => {}
                            Read::Through if coarse_class(&ty).is_some() => {}
                            Read::Through => return Err(hidden(&op, &class, &ty)),
                            Read::Observes => match exact(operand) {
                                Some(e) => *operand = e,
                                None => return Err(observed(&op, &class)),
                            },
                        }
                    }
                }
            }
            m.values_mut().try_for_each(walk)
        }
        Value::Array(a) => a.iter_mut().try_for_each(walk),
        _ => Ok(()),
    }
}

/// `v` read through `q_exact_<type>(<its spelling>, <the values it reads>)`, or `None` if `v` reads a
/// value of a type whose `=` is not identity from a row: a column, a subquery, an aggregate.
fn exact(v: &Value) -> Option<Value> {
    let mut inputs = Vec::new();
    let mut spelling = fixed(v, &mut inputs)?;
    rename_emitted_types(&mut spelling);
    let ty = ty_of(v);
    let mut operand = vec![json!({ "operator": spelling.to_string(), "operand": [], "type": "VARCHAR" })];
    operand.extend(inputs);
    Some(json!({
        "operator": format!("q_exact_{}", name_part(&ty).to_lowercase()),
        "operand": operand,
        "type": ty,
    }))
}

/// The spelling of `v`, with each value of a type whose `=` is identity that `v` reads from a row
/// replaced by `{"input": k}` and pushed onto `inputs`. Such a value has one spelling per class, so
/// equal inputs are the same input. `None` where `v` reads a value whose `=` is not identity from a
/// row.
fn fixed(v: &Value, inputs: &mut Vec<Value>) -> Option<Value> {
    if coarse(v).is_none() {
        if closed(v) {
            return Some(v.clone());
        }
        inputs.push(v.clone());
        return Some(json!({ "input": inputs.len() - 1 }));
    }
    let m = v.as_object()?;
    if m.contains_key("column") || m.contains_key("query") {
        return None;
    }
    let operands = m.get("operand")?.as_array()?;
    let spelled = operands.iter().map(|o| fixed(o, inputs)).collect::<Option<Vec<_>>>()?;
    let mut copy = v.clone();
    copy["operand"] = Value::Array(spelled);
    Some(copy)
}

/// Whether `v` reads nothing from a row: no column, no subquery.
fn closed(v: &Value) -> bool {
    match v {
        Value::Object(m) => !m.contains_key("column") && !m.contains_key("query") && m.values().all(closed),
        Value::Array(a) => a.iter().all(closed),
        _ => true,
    }
}

/// Two values `=` calls equal and that print differently, for a refusal's message.
fn example(class: &str) -> String {
    let element = class.trim_end_matches("[]");
    let e = match element {
        "numeric" => "2.0 and 2.00",
        "float" => "-0 and 0",
        "interval" => "'1 day' and '24 hours'",
        "jsonb" => r#"'{"a": 1.0}' and '{"a": 1.00}'"#,
        _ => "two values that print differently",
    };
    if element.len() < class.len() {
        format!("arrays holding {e}")
    } else {
        e.to_string()
    }
}

fn observed(op: &str, class: &str) -> FrontendError {
    unsupported(format!(
        "a value of type {class} read by {op}, which is not known to give equal results on values = calls \
         equal, as it does {}",
        example(class)
    ))
}

fn hidden(op: &str, class: &str, ty: &str) -> FrontendError {
    unsupported(format!(
        "a value of type {class} passed through {op}, whose result type {ty} would not say that = calls {} equal",
        example(class)
    ))
}
