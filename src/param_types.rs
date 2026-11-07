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
//! One function reads an untyped parameter as a text rather than at its company's type:
//! `substring(s from $1)` is Postgres's regular-expression form, `substring(text, text)`, since an
//! unknown argument resolves to the string category where a candidate accepts one, and only
//! `substring(s from $1::integer)` is the position form. So an inferred INTEGER parameter that is
//! the position or the length of a `SUBSTRING` is refused as well, whatever its other uses.
//!
//! Only parameters whose type inference gave are checked. A `declare` line that types `QP<n>` stands
//! for a client that declared the parameter's type, which Postgres takes as given.

use std::collections::{BTreeMap, BTreeSet};

use serde_json::Value;

use crate::error::{unsupported, Result};
use crate::types::{coarse_class, opaque_identity, ty_of, untag, IDENTITY_OPAQUE};

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
                } else if op == "SUBSTRING" {
                    // The position form only: `lower` refuses every other argument type, and the
                    // parameter here is typed INTEGER. Postgres types it as a text if this is its
                    // first use, and takes the regular-expression form.
                    if let Some(p) = operand.iter().skip(1).find_map(|o| narrow_param(o, inferred)) {
                        return Err(unsupported(format!(
                            "{p}, inferred as an integer, is the position or length of a SUBSTRING: Postgres \
                             types an untyped parameter there as a text, and takes the regular-expression \
                             form of substring"
                        )));
                    }
                } else if op == "CAST" || op.starts_with("q_cast_") {
                    // A coercion into an opaque type is a named cast (`types::cast_into`), and so is a
                    // cast to a qualified or opaque target: each is a `CAST` the IR spells by name.
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

/// The comparisons that decide a value's type from the column it meets, and that `sqleq-solver`
/// reads as identity when both operands' `=` is: an untyped parameter here takes the column's type.
const EQUALITIES: &[&str] = &["=", "<>", "IS DISTINCT FROM", "IS NOT DISTINCT FROM"];

/// The parameters whose `=` is identity: those a lowered pair compares with columns of one opaque
/// type whose `=` is identity, and with no other column. Read on the queries as lowered, before
/// [`crate::types::rename_emitted_types`] erases the declared type a column's type carries.
///
/// Type inference merges every opaque type into one, so a parameter compared with a `uuid` column
/// is declared plain VARBINARY, an opaque value whose `=` `sqleq-solver` does not take to be
/// identity, and `fact.version = $1` against `version.id = $1` is compared through a key that no
/// substitution can see through. But Postgres reads an untyped parameter compared with a column at
/// that column's type, and a second use at a type with no `=` against it is an error, which no
/// running query has. So a parameter that every column comparison sets against a column of the same
/// type, one whose `=` is identity ([`opaque_identity`], the rule the catalog vouches for columns
/// by: a `uuid`, a `bytea`), holds a value of that type, and its `=` is identity too. One
/// comparison with a column of another type, or of one whose `=` is not identity (`jsonb`, a float,
/// an unvouched opaque), leaves the parameter out. The two queries are read together, as inference
/// reads them, since one `$N` is one value in both.
pub fn identity_params(input: &Value) -> BTreeSet<String> {
    let mut met: BTreeMap<String, Option<String>> = BTreeMap::new();
    for q in input["queries"].as_array().into_iter().flatten() {
        meet(q, &mut met);
    }
    met.into_iter().filter_map(|(p, ty)| ty.map(|_| p)).collect()
}

/// Records, per parameter, the declared type of every column a comparison sets it against: `None`
/// once two disagree or one is not an opaque type whose `=` is identity.
fn meet(v: &Value, met: &mut BTreeMap<String, Option<String>>) {
    match v {
        Value::Object(m) => {
            let op = m.get("operator").and_then(Value::as_str).unwrap_or("");
            if let Some([a, b]) = m.get("operand").and_then(Value::as_array).map(Vec::as_slice) {
                if EQUALITIES.contains(&op) {
                    for (p, c) in [(a, b), (b, a)] {
                        if let (Some(name), true) = (carrier(p), c.get("column").is_some()) {
                            let ty = identity_type(c);
                            let seen = met.entry(name).or_insert_with(|| ty.clone());
                            if *seen != ty {
                                *seen = None;
                            }
                        }
                    }
                }
            }
            m.values().for_each(|x| meet(x, met));
        }
        Value::Array(a) => a.iter().for_each(|x| meet(x, met)),
        _ => {}
    }
}

/// The `QP<n>` a node is, if it is a bare parameter carrier of an opaque type.
fn carrier(v: &Value) -> Option<String> {
    let name = v.get("operator")?.as_str()?;
    let n = name.strip_prefix("QP")?;
    let opaque = ty_of(v).starts_with("VARBINARY");
    (!n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()) && opaque).then(|| name.to_string())
}

/// The declared type of a column node whose type is the opaque one with an identity `=`, as the
/// catalog vouches for a column ([`IDENTITY_OPAQUE`] carrying a type [`opaque_identity`] lists).
fn identity_type(column: &Value) -> Option<String> {
    let raw = column.get("type")?.as_str()?;
    let (_, declared) = raw.split_once('@')?;
    (untag(raw) == IDENTITY_OPAQUE && opaque_identity(declared)).then(|| declared.to_string())
}
