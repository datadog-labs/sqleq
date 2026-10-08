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
//! For some types the class has one member: the integers, text, `boolean`, the temporal types but
//! `interval`, and the opaque types on [`crate::types::opaque_identity`]'s list (`bytea`, `uuid`,
//! arrays of the others). For `numeric`, the floats, `interval` and `jsonb` it has more: `2.0 =
//! 2.00`, `-0 = 0`, `'1 day' = '24 hours'` (and `'1 mon' = '30 days'`), and `'{"a": 1.0}' =
//! '{"a": 1.00}'`, and an array of one of them compares its elements the same way. For any other
//! type it may: `numrange` compares its bounds as `numeric`s, a domain over `numeric` is a
//! `numeric`, and the result of a function nobody declared may be either (`round(i, 1)` is a
//! `numeric`, `sqrt(i)` a float). So identity is what has to be established, and a type the frontend
//! does not know is one whose `=` is not identity ([`crate::types::coarse_class`]).
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
//!   it with [`COARSE_OPAQUE`](crate::types::COARSE_OPAQUE) where it would otherwise be VARBINARY,
//!   so a read of it further up, in another query block included, is sorted the same way. Over a
//!   value of a type the frontend does not know, fewer operations are known to: the ones that
//!   compare it, count it, return it or cast it to a number ([`unknown`]), since all that is known
//!   of such a type is that its `=` is an equivalence relation.
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
//! An `UPDATE` or an `INSERT` that stores such a value in a column of another type applies a cast
//! the IR does not spell out, and [`crate::dml`] reads it by the same rule ([`stores_by_value`],
//! [`read_stored`]), without that exception: it says why.
//!
//! What a class cannot stand for is a value whose `=` is not an equivalence relation: `box`,
//! `circle`, `lseg` and `line` compare within a tolerance, so `a = b` and `b = c` do not make
//! `a = c`, and no operation needs to observe anything for a prover's reading to be wrong. Those are
//! [`crate::types::UNFAITHFUL`], refused wherever they reach a plan, and so is the result of a core
//! function that returns one ([`call_type`]). A type a user or an extension defines is taken to have
//! an `=` that is an equivalence relation, as a btree operator class makes it.

use serde_json::{json, Value};

use crate::error::{unsupported, FrontendError, Result};
use crate::types::{
    coarse_class, known_class, name_part, opaque_of_class, rename_emitted_types, ty_of, unfaithful_result,
    IDENTITY_OPAQUE, UNKNOWN_CLASS,
};

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

/// The [`PASS_THROUGH`] functions that return one of their arguments. Over a value of a type the
/// frontend does not know they still read it by its class ([`unknown`]).
const SELECTS: [&str; 4] = ["COALESCE", "NULLIF", "GREATEST", "LEAST"];

/// The [`PASS_THROUGH`] functions whose result is of their first argument's type, or of the common
/// type of all of them: the [`SELECTS`], and `->`, `#>` and `json_extract_path`, which take a part
/// of a `json` as a `json` (and of a `jsonb` as a `jsonb`). Their result's `=` is identity where
/// every argument's is. `round(i)` is not here: it is a float.
const SAME_TYPE: [&str; 7] =
    ["COALESCE", "NULLIF", "GREATEST", "LEAST", "Q_OP_JSONX", "Q_OP_JSONPATH", "Q_OP_JSONPATH_ELEMS"];

/// The type of a call to `name` over `operand`, given the type `ret` it would otherwise have. A
/// function nobody declared returns VARBINARY, a value of a type the frontend does not know, whose
/// `=` is not taken to be identity. Four calls say more:
///
/// * a core function that returns `box`, `circle`, `lseg` or `line` returns that type
///   ([`crate::types::unfaithful_result`]), whose `=` is not even transitive;
/// * a symbol whose name says it returns text or a boolean (`q_str_jsonx`, which `->>` becomes, and
///   the other `q_str_` and `q_bool_` names) returns a value whose `=` is identity,
///   [`IDENTITY_OPAQUE`];
/// * a [`PASS_THROUGH`] function over a value whose `=` is not identity returns a value of the same
///   kind, so `coalesce(x, 0)` over a `numeric` `x` gets `x`'s class as a
///   [`COARSE_OPAQUE`](crate::types::COARSE_OPAQUE) name;
/// * one of [`SAME_TYPE`] over values whose `=` is identity returns one, [`IDENTITY_OPAQUE`].
///
/// VARBINARY to the provers in all but the first, which is refused unless the two plans are one.
pub fn call_type(name: &str, operand: &[Value], ret: String) -> String {
    if ret != "VARBINARY" {
        return ret;
    }
    if let Some(t) = unfaithful_result(name) {
        return t.to_string();
    }
    if matches!(crate::infer::builtin_rtype(name), Some(crate::infer::Ty::Str | crate::infer::Ty::Bool)) {
        return IDENTITY_OPAQUE.to_string();
    }
    if PASS_THROUGH.contains(&name) {
        let types: Vec<String> = operand.iter().map(ty_of).collect();
        let classes = || types.iter().filter_map(|t| coarse_class(t));
        if let Some(class) = classes().find(|c| known_class(c)).or_else(|| classes().next()) {
            return opaque_of_class(class);
        }
        if SAME_TYPE.contains(&name) && !operand.is_empty() {
            return IDENTITY_OPAQUE.to_string();
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
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
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
            t if coarse_class(t).is_some_and(known_class) => Read::Through,
            _ => Read::Observes,
        },
        _ => Read::Observes,
    }
}

/// How the operation `op`, which [`reads`] reads a `numeric` as `read`, reads a value of a type the
/// frontend does not know ([`known_class`] is false for its class): a column of a type no reader
/// names, a domain over one, the result of a function nobody declared.
///
/// All that is known of such a type is that its `=` is an equivalence relation, and that its order,
/// where it has one, agrees with it, as a btree operator class makes them. So a comparison, a null
/// test, `count`, `min` and `max` still read it by its class, and so do the operations that return
/// one of their operands or a part of an array ([`SELECTS`], `CASE`, a subscript, an array
/// constructor). So does a cast to a number or a boolean: every type Postgres 17 casts to one (the
/// integers, `numeric`, the floats, `boolean`, `money`, `jsonb`, `bit`, `"char"`, `oid` and the
/// `reg` types, and text through its input function) converts by value, and a cast a user defines
/// is taken to, as its order is. Everything else that [`reads`]
/// knows to read a `numeric` by its value, `+`, `round`, `@>`, `sum`, is a function of the value's
/// class only because of what it is over a `numeric`, and is not known to be one here.
fn unknown(op: &str, read: Read) -> Read {
    let by_order = !matches!(op, "Q_BOOL_CONTAINS" | "Q_BOOL_OVERLAP");
    let selects = SELECTS.contains(&op)
        || op == "CASE"
        || op.starts_with("q_subscript_")
        || op.starts_with("q_array_")
        || is_cast(op);
    match read {
        Read::Value if by_order => Read::Value,
        Read::Through if selects => Read::Through,
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
                let known = reads(&op, &ty, &types);
                if let Some(Value::Array(operands)) = m.get_mut("operand") {
                    for operand in operands.iter_mut() {
                        let Some(class) = coarse(operand) else { continue };
                        let read = if known_class(&class) { known } else { unknown(&op, known) };
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

/// What a refusal calls a value of the class `class`.
fn value_of(class: &str) -> String {
    if class == UNKNOWN_CLASS {
        "a value of a type the frontend does not know (an opaque column, or the result of a function nobody \
         declared)"
            .to_string()
    } else {
        format!("a value of type {}", class.to_lowercase())
    }
}

/// The refusal of a value of type `class` read by `op`, an operation not known to give equal results
/// on values `=` calls equal.
pub fn observed(op: &str, class: &str) -> FrontendError {
    if !known_class(class) {
        return unsupported(format!(
            "{} read by {op}, which is not known to give equal results on values = calls equal; that type's = \
             is not known to be identity",
            value_of(class)
        ));
    }
    unsupported(format!(
        "a value of type {class} read by {op}, which is not known to give equal results on values = calls \
         equal, as it does {}",
        example(class)
    ))
}

fn hidden(op: &str, class: &str, ty: &str) -> FrontendError {
    unsupported(format!(
        "{} passed through {op}, whose result type {ty} would not say that = calls {} equal",
        value_of(class),
        example(class)
    ))
}

/// Whether Postgres stores a value of a type whose `=` is not identity in a column of type `ty`
/// through a cast that gives equal results on values `=` calls equal: the assignment cast an `UPDATE`
/// or an `INSERT` applies and the IR does not spell out ([`crate::dml`]). Judged as [`reads`] judges
/// an explicit cast: one to a number type, to a boolean or to another type whose `=` is not
/// identity converts by value, and one to text, `json` or anything else is not known to.
///
/// `coerces` says the column's declared type carries a modifier that does not convert by value,
/// which the prover type `ty` cannot say: an interval's fields or precision. A `numeric` column's
/// scale rounds the value it is given, which does: `2.0` and `2.00` both become `2.00` in a
/// `numeric(10,2)`.
pub fn stores_by_value(ty: &str, coerces: bool) -> bool {
    !coerces && reads("CAST", ty, &[]) != Read::Observes
}

/// `v`, a value an `UPDATE` or an `INSERT` stores through a cast not known to give equal results on
/// values `=` calls equal ([`stores_by_value`]), read as [`refuse_observed`] reads the operand of
/// such a cast: through `q_exact_<type>` where its spelling fixes it, and refused otherwise, `cast`
/// naming the cast. A value whose `=` is identity, a NULL and a `q_exact_` term are left alone.
pub fn read_stored(v: &mut Value, cast: &str) -> Result<()> {
    let Some(class) = coarse(v) else { return Ok(()) };
    match exact(v) {
        Some(e) => {
            *v = e;
            Ok(())
        }
        None => Err(observed(cast, &class)),
    }
}
