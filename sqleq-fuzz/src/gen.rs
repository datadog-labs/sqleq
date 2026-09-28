// Unless explicitly stated otherwise all files in this repository are licensed under the
// Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/).
// Copyright 2026-Present Datadog, Inc.

//! Random value generation and SQL literal rendering.
//!
//! Values are drawn from a tiny domain (ints {0,1,2}, doubles {0,1,2}, chars {a,b,c}, a few
//! dates/timestamps/uuids, both booleans) so equality filters and joins actually collide across the
//! small instances — that collision is what surfaces DISTINCT / LIMIT / filter differences.

use rand::rngs::StdRng;
use rand::seq::SliceRandom;
use rand::Rng;

use crate::schema::{Column, VType};
use crate::typing::Need;

/// A concrete generated value (or SQL NULL).
#[derive(Clone, Debug, PartialEq)]
pub enum Val {
    Null,
    Int(i64),
    Dbl(f64),
    Bool(bool),
    Str(String),
    Date(String),
    Ts(String),
    /// Held apart from `Str` so it renders with its cast: DuckDB compares a UUID only against a
    /// UUID inside `IN`/`ANY`/`ALL`, and a bare string literal there is a binder error.
    Uuid(String),
    /// An array value: a param bound as `col = ANY($N)`, or the data of a column the DDL declares
    /// as `text[]`. Never nested — Postgres multi-dimensional arrays are not modelled.
    List(Vec<Val>),
}

/// The uuid domain, shared by column data and by `$N::uuid` params so the two can match.
const UUIDS: [&str; 3] = [
    "00000000-0000-0000-0000-000000000001",
    "00000000-0000-0000-0000-000000000002",
    "00000000-0000-0000-0000-000000000003",
];

/// The JSON document domain, shared by column data and by `$N::json` params so the two can match.
///
/// **One canonical spelling per document, and keys in sorted order.** DuckDB compares JSON
/// *textually*: `'{"a": 1}'::json = '{"a":1}'::json` and `'{"a":1,"b":2}' = '{"b":2,"a":1}'` are
/// both FALSE on 1.5.5. So two spellings of one document in this pool would be two values that
/// compare unequal while denoting the same thing -- which is not merely unselective but unsound
/// against Postgres, whose `jsonb` equality normalises. With a single spelling per document,
/// textual and semantic equality coincide over everything we generate.
///
/// The keys are the `a`/`b`/`c` of [`Need::JsonKey`] and the values live in `randval`'s integer
/// `{0,1,2}` and varchar `{a,b,c}` domains, so an accessor's result can still equal a generated
/// scalar or a bound param -- `j ->> 'k' = $1` selects rows instead of never matching. Each key is
/// absent from two documents, an integer in one and a string in one, which keeps `IS NULL`,
/// `(j ->> 'k')::int` and a plain string comparison all discriminating.
///
/// `{}` is in the pool on purpose: it is the only way `j ->> 'k' IS NULL` holds without the column
/// itself being NULL (a NOT NULL json column would otherwise never satisfy it). A non-object
/// document earns no place beside it -- `'[]'::json ->> 'a'` and `'[1,2]'::json ->> 'a'` are both
/// NULL, the same answer `{}` already gives.
pub const JSONS: [&str; 4] = [
    "{}",
    r#"{"a":1,"b":"b"}"#,
    r#"{"a":"a","c":2}"#,
    r#"{"b":2,"c":"c"}"#,
];

/// Generate a value for a column of type `vt`. Nullable columns are NULL with probability 0.3.
pub fn randval(vt: VType, nullable: bool, rng: &mut StdRng) -> Val {
    if nullable && rng.gen_bool(0.3) {
        return Val::Null;
    }
    match vt {
        VType::Integer => Val::Int(*[0i64, 1, 2].choose(rng).unwrap()),
        VType::Double => Val::Dbl(*[0.0f64, 1.0, 2.0].choose(rng).unwrap()),
        VType::Boolean => Val::Bool(*[true, false].choose(rng).unwrap()),
        VType::Date => Val::Date(
            (*["2020-01-01", "2020-01-02", "2020-01-03"]
                .choose(rng)
                .unwrap())
            .to_string(),
        ),
        VType::Timestamp => Val::Ts(
            (*[
                "2020-01-01 00:00:00",
                "2020-01-02 00:00:00",
                "2020-01-03 00:00:00",
            ]
            .choose(rng)
            .unwrap())
            .to_string(),
        ),
        VType::Varchar => Val::Str((*["a", "b", "c"].choose(rng).unwrap()).to_string()),
        VType::Uuid => Val::Uuid((*UUIDS.choose(rng).unwrap()).to_string()),
        // A plain string: DuckDB casts a VARCHAR literal to JSON implicitly, on insert and in a
        // comparison alike, so no `Val` variant of its own is needed the way `Uuid` needs one.
        VType::Json => Val::Str((*JSONS.choose(rng).unwrap()).to_string()),
    }
}

/// Generate a value for a column, wrapping it in a list when the DDL declares the column an array.
///
/// **No NULL among the elements**, which is load-bearing rather than incidental: DuckDB reports
/// `[NULL] @> [NULL]` and `['a', NULL] <@ ['a', NULL]` as TRUE, while Postgres defines containment
/// through `=`, so a NULL element never matches and both are FALSE. A counterexample resting on
/// that divergence would be a DuckDB artifact wearing the costume of one. Excluding NULL elements
/// makes the divergence unreachable instead of arguing about it. The array *as a whole* is still
/// NULL at the usual rate for a nullable column — verified to agree (`NULL::VARCHAR[] && ['a']` is
/// NULL in both).
///
/// Width 1-3 for the same reason [`crate::pair`] draws array params that way: wide enough that the
/// generated arrays actually overlap across the small instances, narrow enough that an overlap
/// predicate stays selective and can still tell the two sides apart.
pub fn randval_col(c: &Column, rng: &mut StdRng) -> Val {
    if !c.array {
        return randval(c.vt, !c.notnull, rng);
    }
    if !c.notnull && rng.gen_bool(0.3) {
        return Val::Null;
    }
    let k = rng.gen_range(1..=3);
    Val::List((0..k).map(|_| randval(c.vt, false, rng)).collect())
}

/// What an explicit `$N::type` cast requires. Postgres types with no `VType` analogue are generated
/// as strings that survive the cast (`'1 day'::interval`), which is why they are separate variants.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CastTarget {
    V(VType),
    Interval,
    Json,
    Time,
}

/// Classify a raw SQL cast type name. `None` for shapes we will not guess at (arrays, structs, and
/// user-defined/enum types) — guessing there would only trade one bind error for another.
pub fn cast_target(ty: &str) -> Option<CastTarget> {
    let lowered = ty.to_lowercase();
    let t = lowered.trim().trim_matches('"');
    // `int ARRAY` / `int ARRAY[4]` are the SQL-standard spellings of `int[]`; the leading word alone
    // would read them as `int`.
    let array_word = t.split_whitespace().any(|w| w == "array" || w.starts_with("array["));
    if t.ends_with("[]") || array_word || t.contains("record") || t.contains("struct") {
        return None;
    }
    let base = t
        .split(|c: char| c == '(' || c.is_whitespace())
        .next()
        .unwrap_or(t);
    // Order matters: `interval` would otherwise be caught by the `int` test, and `timestamp` by
    // the `time` one.
    Some(if base.contains("interval") {
        CastTarget::Interval
    } else if base.contains("uuid") {
        // A real generation domain of its own, so a uuid *column* can satisfy a uuid cast.
        CastTarget::V(VType::Uuid)
    } else if base.contains("json") {
        CastTarget::Json
    } else if base.contains("bool") {
        CastTarget::V(VType::Boolean)
    } else if base == "date" {
        CastTarget::V(VType::Date)
    } else if base.starts_with("timestamp") || base == "datetime" {
        CastTarget::V(VType::Timestamp)
    } else if base.starts_with("time") {
        CastTarget::Time
    } else if base.contains("int") || base.contains("serial") {
        CastTarget::V(VType::Integer)
    } else if ["numeric", "decimal", "real", "double", "float", "money"].contains(&base) {
        CastTarget::V(VType::Double)
    } else if [
        "text",
        "varchar",
        "char",
        "bpchar",
        "character",
        "citext",
        "name",
    ]
    .contains(&base)
    {
        CastTarget::V(VType::Varchar)
    } else {
        return None;
    })
}

/// The element type named by an array cast (`uuid[]` -> `uuid`). `cast_target` deliberately refuses
/// `[]`-suffixed names, so an array-valued param has to be classified by its element type instead.
pub fn array_element_type(ty: &str) -> String {
    ty.replace(['[', ']'], " ").trim().to_string()
}

/// Generate a value that satisfies an explicit cast target.
pub fn randval_cast(ct: CastTarget, rng: &mut StdRng) -> Val {
    let pick = |xs: &[&str], rng: &mut StdRng| Val::Str((*xs.choose(rng).unwrap()).to_string());
    match ct {
        CastTarget::V(v) => randval(v, false, rng),
        CastTarget::Interval => pick(&["1 day", "2 hours", "7 days"], rng),
        // The same pool the column data is drawn from, which is the whole point of sharing it:
        // `j = $1::json` can only ever match if the param is spelled exactly as the column is.
        CastTarget::Json => pick(&JSONS, rng),
        CastTarget::Time => pick(&["00:00:00", "12:00:00", "23:59:59"], rng),
    }
}

/// A value satisfying a syntactic [`Need`].
///
/// The five string domains are closed sets rather than free text, because DuckDB checks the *content*
/// of these strings and not merely their type: an unknown time zone or an unrecognised date field is a
/// hard error, so `'c'` would trade the mistyped-parameter error for an invalid-value one. A pattern
/// and a JSON key are unconstrained in principle, but a random single character would match nothing —
/// these domains are chosen so the predicate can still select rows and thus still discriminate.
pub fn randval_need(need: Need, rng: &mut StdRng) -> Val {
    let pick = |xs: &[&str], rng: &mut StdRng| Val::Str((*xs.choose(rng).unwrap()).to_string());
    match need {
        Need::Type(ct) => randval_cast(ct, rng),
        Need::TimeZone => pick(&["UTC", "America/New_York", "Europe/Berlin"], rng),
        Need::DateField => pick(&["day", "month", "year", "hour"], rng),
        Need::JsonKey => pick(&["a", "b", "c"], rng),
        Need::LikePattern => pick(&["%", "%a%", "a%"], rng),
        Need::Fraction => Val::Dbl(*[0.0, 0.25, 0.5, 0.75, 1.0].choose(rng).unwrap()),
        // Regex wildcards, not LIKE ones: `%` here is a literal per-cent and would match nothing.
        Need::Regex => pick(&[".*", "a", "^a"], rng),
        // Half canonical, half not, and every element a legal integer literal so the `::int4` side
        // always binds. The domain is deliberately coordinated with `randval`'s integer domain
        // above: each non-canonical spelling names a value an integer column actually takes, since
        // a witness whose numeric value no column ever holds would never match a row and would
        // find nothing. Keeping the canonical spellings in play matters too -- a domain that only
        // ever disagreed would refute every pair it touched and so would hide a bug in this very
        // rule.
        Need::NumericString => pick(&["0", "1", "2", "00", "01", "02"], rng),
    }
}

/// Render a value as a SQL literal.
pub fn lit(v: &Val) -> String {
    match v {
        Val::Null => "NULL".to_string(),
        Val::Bool(b) => if *b { "TRUE" } else { "FALSE" }.to_string(),
        Val::Int(i) => i.to_string(),
        // `{:?}` keeps the decimal point (1.0 -> "1.0") so the literal stays a floating value.
        Val::Dbl(d) => format!("{d:?}"),
        Val::Date(s) => format!("DATE '{s}'"),
        Val::Ts(s) => format!("TIMESTAMP '{s}'"),
        Val::Str(s) => format!("'{}'", s.replace('\'', "''")),
        // The cast is load-bearing: `uuid_col = ANY(['...'])` is a binder error (UUID vs VARCHAR),
        // and it is exactly the shape this type exists to make runnable.
        Val::Uuid(s) => format!("'{s}'::UUID"),
        // DuckDB's list literal, which it accepts for `= ANY(...)` / `<> ALL(...)` with the same
        // semantics as Postgres (verified against `IN`/`NOT IN`, including NULL elements).
        Val::List(vs) => format!("[{}]", vs.iter().map(lit).collect::<Vec<_>>().join(", ")),
    }
}

#[cfg(test)]
mod tests {
    use rand::SeedableRng;

    use super::*;

    /// The load-bearing property of [`randval_col`]: no NULL *element*. DuckDB and Postgres disagree
    /// on containment with a NULL element (`[NULL] @> [NULL]` is TRUE in DuckDB, FALSE in Postgres,
    /// which defines containment through `=`), so a counterexample resting on one would be a claim
    /// about DuckDB rather than about the rewrite. The array as a whole may still be NULL.
    #[test]
    fn generated_arrays_never_contain_a_null_element() {
        let c = Column {
            name: "tags".into(),
            vt: VType::Varchar,
            notnull: false,
            array: true,
        };
        let mut rng = StdRng::seed_from_u64(7);
        let mut saw_list = 0;
        let mut saw_null = 0;
        for _ in 0..500 {
            match randval_col(&c, &mut rng) {
                Val::List(elems) => {
                    saw_list += 1;
                    assert!(
                        !elems.is_empty(),
                        "empty list makes overlap predicates vacuous"
                    );
                    assert!(elems.len() <= 3, "{elems:?}");
                    assert!(
                        elems.iter().all(|e| *e != Val::Null),
                        "NULL element: {elems:?}"
                    );
                }
                Val::Null => saw_null += 1,
                other => panic!("array column generated a scalar: {other:?}"),
            }
        }
        assert!(
            saw_list > 0 && saw_null > 0,
            "lists {saw_list}, nulls {saw_null}"
        );

        // NOT NULL removes the whole-value NULL, not just the elements.
        let c = Column { notnull: true, ..c };
        for _ in 0..200 {
            let v = randval_col(&c, &mut rng);
            assert!(
                matches!(v, Val::List(_)),
                "NOT NULL array column produced {v:?}"
            );
        }

        // A scalar column is untouched by the new path.
        let c = Column {
            name: "n".into(),
            vt: VType::Integer,
            notnull: true,
            array: false,
        };
        assert!(matches!(randval_col(&c, &mut rng), Val::Int(_)));
    }
}
