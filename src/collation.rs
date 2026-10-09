// Unless explicitly stated otherwise all files in this repository are licensed under the
// Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/).
// Copyright 2026-Present Datadog, Inc.

//! Collations: which string order a comparison uses, and what the input says about it.
//!
//! Postgres orders two strings by the collation of their comparison: the one an operand names with
//! `COLLATE`, else the one a column declares, else the database's default. Under `en_US.utf8`
//! `'a' < 'B'`; under `C`, `'B' < 'a'`. No input states the database's default, so the frontend
//! does not know it, and only two collations are known to order strings by code point, the order
//! the provers' strings have: `C` and `POSIX`.
//!
//! # Order
//!
//! So an order comparison of two strings (`<`, `<=`, `>`, `>=`, and `BETWEEN`, which is two of
//! them) is native only when its collation is `C` or `POSIX`. Under any other it is an
//! uninterpreted predicate ([`compare`]): `q_str_lt(a, b)` for `a < b` and `q_str_le(a, b)` for
//! `a <= b`, with `a > b` read as `b < a`, under the database's default collation; and the same with
//! a third operand, the collation's name as a string constant, under a named one
//! (`q_str_lt(a, b, 'en_US.utf8')`). The same comparison on both sides still meets, and `a > b`
//! still meets `b < a`; what a prover can no longer do is order two constants, or chain two
//! comparisons into a contradiction, by code point.
//!
//! One predicate per collation, not one for all: two collations order the same two strings
//! differently, so `a < 'x'` under `en_US.utf8` and `b < 'x'` under the default, with `a = b`, are
//! not one predicate.
//!
//! The collation of a comparison is derived as Postgres derives it ([`comparison`]), from what its
//! operands are: a `COLLATE` on an operand wins, then a column's declared collation, and a constant
//! or a parameter contributes the default, which a column's collation overrides (`c < 'x'` over a
//! `COLLATE "C"` column is in `C` order). An operand of any other shape takes its collation from the
//! columns it reads, which the frontend does not trace, so its comparison is refused, unless no
//! column the pair reads declares a collation: every string then has the default one.
//!
//! # Equality
//!
//! Under a deterministic collation -- every predefined one, and a database's default always is
//! one -- `=` holds only between identical strings, which is how the provers read it. Under a
//! non-deterministic one (`CREATE COLLATION ci (..., deterministic = false)`) `'a' = 'A'` can hold.
//! A column under such a collation, or under one the DDL does not create and that is not predefined
//! ([`declared`]), takes the type [`COLLATED`], which is refused wherever a query reads it, as
//! `citext` is. So does a column of a type the IR keeps opaque (an array of text, a domain) under a
//! collation other than the default, whose order no predicate here names.
//!
//! # Everything else that reads a collation
//!
//! Order comparisons are not the only operations a collation decides: `min`, `max`, `greatest` and
//! `least` read its order, and so does a row slice under `ORDER BY`; `upper`, `lower`, `ILIKE` and
//! the regular expressions read its character classes. Each is lowered with no collation in its
//! symbol, which is sound only while every such operation in the pair has one collation. That holds
//! when no column the pair reads declares a collation: every string then has the default one (the
//! `COLLATE` an operand of a comparison may carry decides that comparison and nothing else). A pair
//! that reads a table with such a column is checked after lowering ([`refuse`]): an operation over
//! strings other than the comparisons above and the few that never read a collation ([`BLIND`]) is
//! refused, and so is a slice ordered by a string. Stripping a shared `LIMIT` and `ORDER BY` from
//! both sides is skipped for such a pair, since equal outputs ordered under two collations need not
//! give the same page.

use std::collections::{HashMap, HashSet};

use serde_json::{json, Value};
use sqlparser::ast::{
    visit_relations, ColumnOption, ColumnOptionDef, CreateCollationDefinition, Expr, ObjectName, ObjectNamePart,
    Query, SqlOption, Statement,
};

use crate::catalog::{obj_name, Catalog};
use crate::error::{unsupported, Result};
use crate::scope::Scope;

/// The type a column takes when its collation is one the IR cannot carry; one of
/// [`UNFAITHFUL`][crate::types::UNFAITHFUL].
pub const COLLATED: &str = "COLLATED";

/// A column's collation, as its DDL declares it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Collation {
    /// No `COLLATE`, or `COLLATE "default"`: the database's default collation, which no input names.
    Default,
    /// A deterministic collation, by the last part of its name as Postgres folds it (`"en_US.utf8"`,
    /// `C`). The name is unique among the collations [`declared`] accepts: see [`created`].
    Named(String),
}

impl Collation {
    /// Whether this collation orders strings by code point: `C` or `POSIX`.
    pub fn by_code_point(&self) -> bool {
        matches!(self, Collation::Named(n) if n == "C" || n == "POSIX")
    }
}

/// The collations a DDL creates, by the last part of their folded name, and whether [`declared`] may
/// accept a column under that name: `true` only for a deterministic one that no other `CREATE
/// COLLATION` and no predefined collation shares the name of.
pub type Created = HashMap<String, bool>;

/// An identifier as Postgres folds it: as written when quoted, ASCII-lowercased when not.
fn fold(name: &ObjectName) -> Option<Vec<String>> {
    name.0
        .iter()
        .map(|p| match p {
            ObjectNamePart::Identifier(id) if id.quote_style.is_some() => Some(id.value.clone()),
            ObjectNamePart::Identifier(id) => Some(id.value.to_ascii_lowercase()),
            _ => None,
        })
        .collect()
}

/// A name's last part, and whether it names something in `pg_catalog` (unqualified, or qualified by
/// it), where Postgres finds a predefined collation first.
fn split(name: &ObjectName) -> Option<(String, bool)> {
    let mut parts = fold(name)?;
    let last = parts.pop()?;
    let catalog = match parts.as_slice() {
        [] => true,
        [schema] => schema == "pg_catalog",
        _ => false,
    };
    Some((last, catalog))
}

/// Whether a collation of this name is one Postgres defines itself, every one of them deterministic:
/// `default`, `C`, `POSIX`, `ucs_basic`, `unicode`, the builtin provider's, a libc locale `initdb`
/// imports (`en_US.utf8`, `de_DE`, `C.UTF-8`: a language, a territory, an optional encoding and
/// modifier), or an ICU locale it imports (`en-US-x-icu`, `und-x-icu`).
///
/// A user may name a collation anything, so this cannot know that a name is not one; what it reads
/// is the naming `initdb` gives the ones it creates. A bare `ci` or `nocase` is not predefined.
fn predefined(name: &str) -> bool {
    if matches!(
        name,
        "default" | "C" | "POSIX" | "ucs_basic" | "unicode" | "pg_c_utf8" | "pg_unicode_fast" | "C.utf8" | "C.UTF-8"
    ) {
        return true;
    }
    if let Some(locale) = name.strip_suffix("-x-icu") {
        return !locale.is_empty() && locale.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-');
    }
    // `ll[l]_TT[.encoding][@modifier]`.
    let (rest, modifier) = name.split_once('@').map_or((name, None), |(r, m)| (r, Some(m)));
    let (locale, encoding) = rest.split_once('.').map_or((rest, None), |(l, e)| (l, Some(e)));
    let Some((lang, territory)) = locale.split_once('_') else { return false };
    let word = |s: &str, ok: fn(u8) -> bool| !s.is_empty() && s.bytes().all(ok);
    (2..=3).contains(&lang.len())
        && word(lang, |b| b.is_ascii_lowercase())
        && territory.len() == 2
        && word(territory, |b| b.is_ascii_uppercase())
        && encoding.is_none_or(|e| word(e, |b| b.is_ascii_alphanumeric() || b == b'-'))
        && modifier.is_none_or(|m| word(m, |b| b.is_ascii_alphanumeric()))
}

/// Whether a `CREATE COLLATION`'s options leave it deterministic: no `deterministic` option, or one
/// that is plainly true. Anything else, `false` and every spelling not recognised here, is read as
/// non-deterministic, which only costs a refusal.
fn deterministic(definition: &CreateCollationDefinition) -> bool {
    match definition {
        // A copy has the source's properties; only a predefined source's are known.
        CreateCollationDefinition::From(src) => split(src).is_some_and(|(n, cat)| cat && predefined(&n)),
        CreateCollationDefinition::Options(options) => options.iter().all(|o| match o {
            SqlOption::KeyValue { key, value } if key.value.eq_ignore_ascii_case("deterministic") => {
                let v = value.to_string();
                matches!(v.trim_matches('\'').to_ascii_lowercase().as_str(), "true" | "on" | "yes" | "1")
            }
            _ => true,
        }),
    }
}

/// The collations `statements` create, for [`declared`].
///
/// Read by the last part of the name, the part a column's `COLLATE` most often spells alone. A name
/// created twice, or created with the name of a predefined collation, is marked as one no column may
/// use: which of the two a column's `COLLATE` reaches depends on a search path no input states, and
/// two collations under one name would share one predicate.
pub fn created<'a>(statements: impl IntoIterator<Item = &'a Statement>) -> Created {
    let mut out = Created::new();
    for st in statements {
        let Statement::CreateCollation(c) = st else { continue };
        let Some((name, _)) = split(&c.name) else { continue };
        let usable = deterministic(&c.definition) && !predefined(&name);
        out.entry(name).and_modify(|u| *u = false).or_insert(usable);
    }
    out
}

/// A column's collation from its `COLLATE` clause. `None` for one under which `=` may not be
/// identity: a collation the DDL creates non-deterministic, or one it does not create and that is
/// not [`predefined`] (and so may be either), or a name this cannot read.
pub fn declared(name: &ObjectName, created: &Created) -> Option<Collation> {
    let (last, in_catalog) = split(name)?;
    // What the DDL creates is read first, and a name it creates is never read as the predefined
    // collation of that name: the conservative reading of a search path no input states.
    if let Some(&usable) = created.get(&last) {
        return usable.then_some(Collation::Named(last));
    }
    if !(in_catalog && predefined(&last)) {
        return None;
    }
    Some(if last == "default" { Collation::Default } else { Collation::Named(last) })
}

/// The type and collation a column takes in the catalog, from its options and the type its name
/// maps to.
///
/// A column under a collation [`declared`] does not accept, under two `COLLATE`s, or of a type other
/// than VARCHAR under a collation other than the default, takes [`COLLATED`]. An `UNFAITHFUL` type
/// keeps its own name: it is refused wherever it is read already.
pub fn column(options: &[ColumnOptionDef], ty: String, created: &Created) -> (String, Collation) {
    let mut clauses = options.iter().filter_map(|o| match &o.option {
        ColumnOption::Collation(n) => Some(n),
        _ => None,
    });
    let (Some(name), None) = (clauses.next(), clauses.next()) else {
        let any = options.iter().any(|o| matches!(o.option, ColumnOption::Collation(_)));
        return (if any { COLLATED.to_string() } else { ty }, Collation::Default);
    };
    let unfaithful = crate::types::UNFAITHFUL.iter().any(|(n, _)| *n == ty);
    match declared(name, created) {
        Some(Collation::Default) => (ty, Collation::Default),
        Some(c) if ty == "VARCHAR" => (ty, c),
        _ if unfaithful => (ty, Collation::Default),
        _ => (COLLATED.to_string(), Collation::Default),
    }
}

/// Whether some column of `cat` declares a collation: one other than the default, or one the IR
/// cannot carry ([`COLLATED`]).
pub fn varies(cat: &Catalog) -> bool {
    cat.tables.iter().any(|t| {
        t.collations.iter().any(|c| *c != Collation::Default) || t.cols.iter().any(|(_, ty)| ty == COLLATED)
    })
}

/// `cat` with the collations of the tables neither query names cleared, when there are any to clear.
///
/// A table no query names is a table no query reads, so its collations cannot reach the pair, and
/// clearing them keeps a declared collation elsewhere in the schema from costing the pair the
/// operations [`refuse`] refuses. A name is matched whole and by its last part, so a name that is
/// not a table (a `WITH` binding's) can only keep more tables than needed.
pub fn narrow(cat: &Catalog, queries: &[Query]) -> Option<Catalog> {
    if !varies(cat) {
        return None;
    }
    let mut named = HashSet::new();
    for q in queries {
        let _ = visit_relations(q, |n| {
            let full = obj_name(n);
            named.extend(cat.find(&full));
            named.extend(full.rsplit('.').next().and_then(|last| cat.find(last)));
            std::ops::ControlFlow::<()>::Continue(())
        });
    }
    let mut tables = cat.tables.clone();
    let mut cleared = false;
    for (i, t) in tables.iter_mut().enumerate() {
        if !named.contains(&i) && t.collations.iter().any(|c| *c != Collation::Default) {
            t.collations.iter_mut().for_each(|c| *c = Collation::Default);
            cleared = true;
        }
    }
    cleared.then_some(Catalog { tables, unread: cat.unread.clone() })
}

/// An operand stripped of the `COLLATE` it names, and whether it named one: the operand of a
/// comparison may name `C` or `POSIX`, which decides that comparison's order and nothing else. Any
/// other collation is refused, and so is a `COLLATE` anywhere but on an operand of a comparison
/// (`lower_expr` has no case for it): it can change what `upper` or `ILIKE` above it compute.
pub fn strip(e: &Expr) -> Result<(&Expr, bool)> {
    let mut bare = e;
    while let Expr::Nested(inner) = bare {
        bare = inner;
    }
    let Expr::Collate { expr, collation } = bare else { return Ok((e, false)) };
    match split(collation) {
        Some((n, true)) if n == "C" || n == "POSIX" => Ok((expr, true)),
        _ => Err(unsupported(format!(
            "COLLATE {collation}: only \"C\" and \"POSIX\", which order by code point as the provers do, are modelled"
        ))),
    }
}

/// The collation one operand contributes to its comparison, from what it is: `Some` for a
/// constant or a parameter (the default), and for a column of a base table (its declared one);
/// `None` for anything else, whose collation comes from the columns it reads.
fn operand(cat: &Catalog, scope: &Scope, ast: &Expr, lowered: &Value) -> Option<Collation> {
    let mut e = ast;
    while let Expr::Nested(inner) = e {
        e = inner;
    }
    let constant = |e: &Expr| matches!(e, Expr::Value(_)) || crate::infer::param_index(e).is_some();
    if constant(e) {
        return Some(Collation::Default);
    }
    // A constant cast to a string type is still a constant of that type, with its default collation.
    if let Expr::Cast { expr, data_type, .. } = e {
        let mut inner = expr.as_ref();
        while let Expr::Nested(i) = inner {
            inner = i;
        }
        return (constant(inner) && crate::types::map_type(data_type) == "VARCHAR").then_some(Collation::Default);
    }
    if lowered.get("operand").is_some() {
        return None;
    }
    let level = lowered.get("column")?.as_u64()? as usize;
    let (binding, i) = scope.binding_of(level)?;
    cat.tables.get(binding.table?)?.collations.get(i).cloned()
}

/// The collation of a comparison among `operands` (each as written, `COLLATE` stripped, and as
/// lowered): `None` when it orders by code point, as Postgres derives it. `explicit` is whether an
/// operand named one with `COLLATE`, which [`strip`] admits only for `C` and `POSIX`.
///
/// Otherwise the operands' own collations combine: the default yields to a column's, and two
/// columns of two collations cannot be compared (Postgres raises an error), which is refused. An
/// operand [`operand`] cannot place is refused when the catalog [`varies`]; when it does not, every
/// string has the default collation.
pub fn comparison(
    cat: &Catalog,
    scope: &Scope,
    explicit: bool,
    operands: &[(&Expr, &Value)],
) -> Result<Option<Collation>> {
    if explicit {
        return Ok(None);
    }
    let mut found = Collation::Default;
    for (ast, lowered) in operands {
        let c = match operand(cat, scope, ast, lowered) {
            Some(c) => c,
            None if !varies(cat) => Collation::Default,
            None => {
                return Err(unsupported(format!(
                    "an order comparison of {ast}, whose collation comes from the columns it reads, where a \
                     column the pair reads declares one"
                )))
            }
        };
        match (&found, c) {
            (_, Collation::Default) => {}
            (Collation::Default, c) => found = c,
            (f, c) if *f == c => {}
            _ => return Err(unsupported("an order comparison between strings of two collations")),
        }
    }
    Ok(if found.by_code_point() { None } else { Some(found) })
}

/// The comparison `l op r`, as [`make_cmp`][crate::types::make_cmp] builds it, except that an order
/// comparison of two strings is the uninterpreted predicate for its collation (see the module docs)
/// unless `collation` says it orders by code point.
///
/// `collation` is handed the two operands as lowered, before any coercion, and is asked only for an
/// order comparison of two strings, so an operand it cannot place costs nothing anywhere else.
pub fn compare(
    op: &str,
    l: Value,
    r: Value,
    collation: impl FnOnce(&Value, &Value) -> Result<Option<Collation>>,
) -> Result<Value> {
    let order = matches!(op, "<" | "<=" | ">" | ">=");
    let lowered = order.then(|| (l.clone(), r.clone()));
    let v = crate::types::make_cmp(op, l, r);
    let strings = v["operand"].as_array().is_some_and(|a| a.iter().all(|x| x["type"] == "VARCHAR"));
    let Some((l0, r0)) = lowered.filter(|_| strings) else { return Ok(v) };
    let Some(collation) = collation(&l0, &r0)? else { return Ok(v) };
    let [l, r] = [v["operand"][0].clone(), v["operand"][1].clone()];
    let (name, mut operand) = match op {
        "<" => ("q_str_lt", vec![l, r]),
        "<=" => ("q_str_le", vec![l, r]),
        ">" => ("q_str_lt", vec![r, l]),
        _ => ("q_str_le", vec![r, l]),
    };
    if let Collation::Named(n) = collation {
        operand.push(crate::types::string_literal(&n));
    }
    Ok(json!({ "operator": name, "operand": operand, "type": "BOOLEAN" }))
}

/// The operations over strings that never read a collation, the ones [`refuse`] lets through: the
/// comparisons [`compare`] built, `=` and the forms built on it (under a deterministic collation it
/// is identity), the null tests, `||`, `CASE`, `COALESCE`, a cast, `COUNT` and `LIKE` (which matches
/// characters, not their order or case). A function the frontend names (`q_cast_..`, `q_conv_..`,
/// `qcastN`, the JSON lookups) is matched by [`blind`].
pub const BLIND: &[&str] = &[
    "<", "<=", ">", ">=", "q_str_lt", "q_str_le", "=", "<>", "IS DISTINCT FROM", "IS NOT DISTINCT FROM", "IN",
    "= ANY", "<> ALL", "IS NULL", "IS NOT NULL", "||", "CASE", "COALESCE", "NULLIF", "CAST", "COUNT", "LIKE",
    "q_numeric", "q_op_jsonx", "q_str_jsonx", "q_op_jsonpath", "q_str_jsonpath",
];

/// Whether `op` is one of [`BLIND`], or one of the frontend's own casts and conversions, or a row
/// equality.
fn blind(op: &str) -> bool {
    let qcast = op.get(..5).is_some_and(|p| p.eq_ignore_ascii_case("qcast"))
        && op.get(5..).is_some_and(|k| !k.is_empty() && k.bytes().all(|b| b.is_ascii_digit()));
    BLIND.contains(&op)
        || qcast
        || op.starts_with("q_cast_")
        || op.starts_with("q_conv_")
        || op.starts_with("q_row_eq_")
}

/// Refuse a lowered pair that reads a column of the [`COLLATED`] type, and, when `cat` [`varies`],
/// one with an operation over strings that reads a collation its symbol does not name: anything
/// over a string but the [`BLIND`] operations, and a row slice ordered by a string. See the module
/// docs.
///
/// Except where the two queries are `one_plan`: every collation an operation uses is determined by
/// the columns and constants the plan names, so one plan computes one thing however the collations
/// are read. Not where the lowering dropped a subquery's `ORDER BY`, though both sides lower alike:
/// under a collation that calls `'a'` and `'A'` equal, a `DISTINCT` above it keeps whichever that
/// order hands it first. `crate::emit` says when they are one plan.
pub fn refuse(cat: &Catalog, input: &Value, one_plan: bool) -> Result<()> {
    if one_plan {
        return Ok(());
    }
    let varies = varies(cat);
    fn walk(v: &Value, varies: bool) -> Option<crate::error::FrontendError> {
        match v {
            Value::Object(m) => {
                if m.get("type").and_then(Value::as_str) == Some(COLLATED) {
                    return Some(unsupported(
                        "a column whose collation may compare different strings as equal, or one no predicate \
                         can name: one the DDL creates non-deterministic, one it neither creates nor Postgres \
                         predefines, or one on a type the IR keeps opaque",
                    ));
                }
                if varies {
                    let op = m.get("operator").and_then(Value::as_str);
                    let string = |x: &Value| x.get("type").and_then(Value::as_str) == Some("VARCHAR");
                    let operands = m.get("operand").and_then(Value::as_array);
                    if let (Some(op), Some(args)) = (op, operands) {
                        if !blind(op) && args.iter().any(string) {
                            return Some(unsupported(format!(
                                "{op} over a string, where a column the pair reads declares a collation: it may \
                                 read the collation, which its symbol does not name"
                            )));
                        }
                    }
                    let keys = m.get("collation").and_then(Value::as_array);
                    if keys.is_some_and(|k| k.iter().any(|e| e.get(1).and_then(Value::as_str) == Some("VARCHAR"))) {
                        return Some(unsupported(
                            "a row slice ordered by a string, where a column the pair reads declares a collation",
                        ));
                    }
                }
                m.values().find_map(|x| walk(x, varies))
            }
            Value::Array(a) => a.iter().find_map(|x| walk(x, varies)),
            _ => None,
        }
    }
    input.get("queries").and_then(|q| walk(q, varies)).map_or(Ok(()), Err)
}
