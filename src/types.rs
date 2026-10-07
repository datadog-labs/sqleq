// Unless explicitly stated otherwise all files in this repository are licensed under the
// Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/).
// Copyright 2026-Present Datadog, Inc.

//! Type mapping and coercion helpers.
//!
//! The prover's `DataType` is one of INTEGER / REAL / BOOLEAN / VARCHAR or a `Custom(name)` (e.g.
//! VARBINARY, geometry types). We render types as the uppercase strings the prover deserializes.
//!
//! # Temporal types
//!
//! DATE, TIME, TIMESTAMP, TIMESTAMPTZ and INTERVAL are kept apart. A DATE is a count of days, a
//! TIMESTAMP a count of microseconds, and reading both as one integer is what let `ts < d + 1` prove
//! equivalent to `ts <= d`: over integers `x < y + 1` is `x <= y`, but `d + 1` is the next *day*.
//!
//! The rule that keeps this sound: a comparison of two values of the same temporal type stays
//! native, and every *operation* on a temporal value is an uninterpreted function.
//!
//! Comparisons are exact. The values of one temporal type, `-infinity` and `infinity` included, form
//! a bounded total order, so they embed order-preservingly into the integers a prover reads them as.
//!
//! Arithmetic is not exact on those integers, because of the infinities: Postgres leaves `infinity`
//! unchanged under `date + integer`, so `'infinity'::date + 1 = 'infinity'`, and it raises an error
//! for `infinity - date`. Read as integer addition, `d + 1 > d` would hold for every `d` and it does
//! not; nor does `d >= $1 AND d < $1 + 1` mean `d = $1`. So `date ± integer`, `date - date` and all
//! interval arithmetic are [`make_arith`]'s `q_arith_*` functions, and every crossing between two
//! types -- an implicit promotion, an explicit cast, a literal cast -- is a [`convert`] call. Each is
//! an uninterpreted function named after its operands' types: a prover that knows nothing about it
//! can only fail to prove through it, and one that knows its Postgres meaning, infinities included,
//! can interpret the name.
//!
//! # Types a prover's arithmetic or equality would misread
//!
//! A type is mapped onto an IR type only where the IR type's operations are Postgres's. Both provers
//! read REAL as exact rational arithmetic and every type's `=` as equality, so:
//!
//! * `numeric` is REAL: its `+`, `-` and `*` are exact. Its division rounds to a finite scale
//!   (`1 / 3.0 * 3.0` is `0.99…990`), so `/` over REAL is the uninterpreted `q_arith_div_real_real`
//!   ([`make_arith`]). REAL carries no scale, so a numeric turned into text, which shows it, is
//!   refused ([`refuse_unfaithful`]).
//! * `real`, `double precision` and the other floats are opaque (VARBINARY): float addition is not
//!   associative, and arithmetic over any opaque operand is an uninterpreted `q_arith_*` function.
//! * `citext` and the blank-padded `char(n)` are [`UNFAITHFUL`]: their `=` ignores case or trailing
//!   spaces, so two values it calls equal can still be told apart, and a value of either is refused
//!   wherever it reaches the plan ([`refuse_unfaithful`]).
//! * Integer types are matched by name, so `int4range` and `point` are not integers.
//!
//! A string literal compared with, or combined with, a value of another type is read at that type,
//! as Postgres resolves an untyped literal: `a = '01'` over an INTEGER `a` compares with `1`
//! ([`coerce_cmp`]). Casting the column to text instead would compare `'1'` with `'01'` as strings.
//!
//! # Constants
//!
//! A constant is a nullary operator whose name is its value: the provers parse the name by the
//! node's type. Where that reading would say more than the value, the constant is spelled otherwise:
//!
//! * The nullary `NULL` is SQL NULL. Both provers check the name before the type -- QED for any
//!   spelling that lowercases to `null`, `sqleq-solver` for `NULL` -- so no string literal is a
//!   nullary node with that text: [`string_literal`] spells it as a concatenation.
//! * QED parses a REAL constant as an `f32`, and builds its value from the `f32`'s numerator and
//!   denominator as `i32`s. A decimal that is not exactly such an `f32` would be rounded, or would
//!   panic the prover, so [`number_literal`] emits only the exact ones as constants.
//! * QED evaluates a `CAST` over a constant by parsing the constant's text as the target type, so
//!   `CAST('0.1' AS REAL)` is the same rounded `f32`, and `CAST(c AS VARCHAR)` is the text `c` is
//!   spelled with. So a numeric constant is spelled the way Postgres prints it, and [`cast_to`] and
//!   [`lower_cast`] give a string constant QED would misread as a REAL an uninterpreted conversion
//!   instead.

use serde_json::{json, Value};
use sqlparser::ast::DataType;

use crate::error::{unsupported, Result};

/// The temporal types this module keeps apart, as the type strings the rest of the frontend uses.
pub const TEMPORAL: &[&str] = &["DATE", "TIME", "TIMESTAMP", "TIMESTAMPTZ", "INTERVAL"];

/// Whether `t` is one of the [`TEMPORAL`] types.
pub fn is_temporal(t: &str) -> bool {
    TEMPORAL.contains(&t)
}

/// The spelling a type leaves the frontend with. Two differ: TIMESTAMPTZ is emitted as TIMESTAMP,
/// and an [`UNFAITHFUL`] type as VARBINARY.
///
/// QED reads DATE, TIME and TIMESTAMP as its integer sort, which keeps their order and their
/// arithmetic, and any other name as an uninterpreted sort with equality only. A TIMESTAMPTZ is an
/// instant in microseconds, so the integer sort is exact for it too, and emitting it under a name QED
/// does not know would cost every range predicate over one. The two types still never meet in the
/// IR: every crossing between them is a conversion named `q_conv_timestamp_timestamptz` or the
/// reverse, so the time zone that separates them lives in that name, not in the type annotation.
pub fn emitted_type_name(t: &str) -> &str {
    if t == "TIMESTAMPTZ" {
        "TIMESTAMP"
    } else if UNFAITHFUL.iter().any(|(n, _)| *n == t) {
        // A schema's column that no query reads, or a value in a pair whose two plans are one
        // ([`refuse_unfaithful`]): opaque, under the name both provers accept for that.
        "VARBINARY"
    } else {
        t
    }
}

/// Apply [`emitted_type_name`] to every type position of an emitted input: each expression's
/// `type`, each schema's `types`, each `VALUES` block's `schema` and the type in each `sort`
/// collation entry (`[position, type, direction]`). Literals are left alone.
pub fn rename_emitted_types(v: &mut Value) {
    match v {
        Value::Object(m) => {
            for (k, x) in m.iter_mut() {
                match (k.as_str(), &mut *x) {
                    ("type", Value::String(s)) => *s = emitted_type_name(s).to_string(),
                    ("collation", Value::Array(entries)) => {
                        for e in entries.iter_mut() {
                            if let Some(Value::String(s)) = e.get_mut(1) {
                                *s = emitted_type_name(s).to_string();
                            }
                        }
                    }
                    ("types" | "schema", Value::Array(a)) if a.iter().all(Value::is_string) => {
                        for t in a.iter_mut() {
                            if let Value::String(s) = t {
                                *s = emitted_type_name(s).to_string();
                            }
                        }
                    }
                    _ => rename_emitted_types(x),
                }
            }
        }
        Value::Array(a) => a.iter_mut().for_each(rename_emitted_types),
        _ => {}
    }
}

/// Classify an upper-cased type spelling as one of the [`TEMPORAL`] types.
///
/// `Some(None)` is a temporal type we do not model: `TIME WITH TIME ZONE` carries an offset that
/// makes it neither a TIME nor a TIMESTAMP, and it stays opaque (VARBINARY). `None` means the name
/// is not temporal at all. A precision qualifier (`TIMESTAMP(3)`) does not change the class; the
/// callers that care about it, the casts, read it off the spelling themselves.
pub fn temporal_class(upper: &str) -> Option<Option<&'static str>> {
    let s = upper.trim();
    let tz = s.contains("WITH TIME ZONE");
    if s == "DATE" {
        Some(Some("DATE"))
    } else if s.starts_with("TIMESTAMPTZ") || (s.starts_with("TIMESTAMP") && tz) {
        Some(Some("TIMESTAMPTZ"))
    } else if s.starts_with("TIMESTAMP") || s == "DATETIME" || s.starts_with("DATETIME(")
        || s == "SMALLDATETIME"
    {
        Some(Some("TIMESTAMP"))
    } else if s.starts_with("TIMETZ") || (s.starts_with("TIME") && tz) {
        Some(None)
    } else if s == "TIME" || s.starts_with("TIME(") || s.starts_with("TIME WITHOUT") {
        Some(Some("TIME"))
    } else if s.starts_with("INTERVAL") {
        Some(Some("INTERVAL"))
    } else {
        None
    }
}

/// The sqlparser `DataType` a temporal type string names, for the casts the frontend inserts itself
/// (the assignment casts of an `UPDATE`). `None` for anything else.
pub fn temporal_data_type(t: &str) -> Option<DataType> {
    use sqlparser::ast::TimezoneInfo;
    Some(match t {
        "DATE" => DataType::Date,
        "TIME" => DataType::Time(None, TimezoneInfo::None),
        "TIMESTAMP" => DataType::Timestamp(None, TimezoneInfo::None),
        "TIMESTAMPTZ" => DataType::Timestamp(None, TimezoneInfo::WithTimeZone),
        "INTERVAL" => DataType::Interval { fields: None, precision: None },
        _ => return None,
    })
}

/// The scalar classes a type name can be read as, by [`scalar_class`]. Anything else is opaque.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Scalar {
    Int,
    /// `numeric`: exact, so the IR's REAL.
    Numeric,
    /// `real`, `double precision` and the other floats: rounded, so opaque.
    Float,
    Str,
    Bool,
    Binary,
}

/// Classify a type's name by exact spelling: upper-cased, unquoted, without its typmod. `None` is a
/// name this crate gives no meaning to, which every caller reads as opaque.
///
/// One table for both type readers, [`map_type`] for declared columns and casts and
/// `infer::map_type_name` for inference and raw DDL, so the two cannot disagree about a name. It is
/// an allowlist rather than substring tests: `INT4RANGE` and `POINT` contain `INT` and are not
/// integers, and `CITEXT` contains `TEXT` and does not compare like text.
pub fn scalar_class(name: &str) -> Option<Scalar> {
    if name.trim().eq_ignore_ascii_case("\"char\"") {
        return None;
    }
    let canon = name.replace(['"', '`'], "").to_uppercase();
    let base = canon.split('(').next().unwrap_or(&canon).trim();
    Some(match base {
        "INT" | "INTEGER" | "INT2" | "INT4" | "INT8" | "SMALLINT" | "BIGINT" | "TINYINT" | "MEDIUMINT"
        | "SERIAL" | "SERIAL2" | "SERIAL4" | "SERIAL8" | "SMALLSERIAL" | "BIGSERIAL" | "OID" => Scalar::Int,
        "NUMERIC" | "DECIMAL" | "DEC" => Scalar::Numeric,
        "REAL" | "FLOAT" | "FLOAT4" | "FLOAT8" | "DOUBLE" | "DOUBLE PRECISION" => Scalar::Float,
        "TEXT" | "VARCHAR" | "CHARACTER VARYING" | "CHAR VARYING" | "NCHAR VARYING"
        | "NATIONAL CHARACTER VARYING" | "NATIONAL CHAR VARYING" | "NVARCHAR" | "STRING" | "CLOB" => {
            Scalar::Str
        }
        "BOOL" | "BOOLEAN" => Scalar::Bool,
        "BYTEA" | "BLOB" | "BINARY" | "VARBINARY" => Scalar::Binary,
        _ => return None,
    })
}

/// The types whose `=` is not equality on what a query can observe, with the reason. Each is its own
/// name in the catalog and in the lowering, and [`refuse_unfaithful`] refuses any plan that carries a
/// value of one.
///
/// No IR type models them. VARCHAR would compare `'A'` and `'a'`, or `'a'` and `'a '`, as different
/// strings. An opaque type would make their `=` the prover's equality, which substitutes equals for
/// equals: from `t.c = u.c` it concludes `t.c::text = u.c::text`, and over citext `'a'` and `'A'`
/// are equal while their text is not. An array of either compares its elements the same way.
pub const UNFAITHFUL: &[(&str, &str)] = &[
    ("CITEXT", "citext compares case-insensitively"),
    ("BPCHAR", "char(n) compares ignoring trailing spaces"),
];

/// The [`UNFAITHFUL`] type a type name denotes, an array of one included, or `None`.
///
/// The quoted `"char"` is not `char`: it is Postgres's one-byte type, whose `=` is equality, and it
/// is left opaque, like every other name [`scalar_class`] does not know. (It reads a literal by its
/// first byte, so `x = 'ab'` is `x = 'a'`, which is why it is not text either.)
pub fn unfaithful_type(name: &str) -> Option<&'static str> {
    if name.trim().trim_end_matches("[]").eq_ignore_ascii_case("\"char\"") {
        return None;
    }
    let canon = name.replace(['"', '`'], "").to_uppercase();
    let elem = canon.split(['(', '[']).next().unwrap_or(&canon).trim();
    let elem = elem.strip_suffix(" ARRAY").unwrap_or(elem).trim();
    match elem {
        "CITEXT" => Some("CITEXT"),
        "CHAR" | "CHARACTER" | "BPCHAR" | "NCHAR" | "NATIONAL CHAR" | "NATIONAL CHARACTER" => Some("BPCHAR"),
        _ => None,
    }
}

/// Refuse a lowered pair whose plans read something no prover reads the way Postgres does:
///
/// * a value of an [`UNFAITHFUL`] type. Read off the `type` of every expression, so it reaches every
///   way such a value enters a plan: a column, a cast, a function declared to return one. A column
///   of that type that neither query reads appears only in the schema, where it costs nothing.
/// * a `numeric` turned into text ([`numeric_to_text`]). A numeric carries a scale, which its text
///   shows: `1.0` and `1.00` are one number and two strings, and `x * 1.0` and `x * 1.00` are the
///   same REAL to a prover, whose casts are functions of the value. No spelling of the cast says the
///   scale either: `x::text` reads a column in one query and `x * 1.0` behind an alias in another.
///
/// Except where the two queries lowered to one plan. Every node of a plan carries its type, and a
/// literal its spelling, so a plan read with citext's own `=`, or with the scales Postgres computes,
/// still says what the query computes, and two queries with one plan compute the same thing however
/// those are read. The provers' misreading cannot matter to a proof that a plan equals itself.
pub fn refuse_unfaithful(input: &Value) -> Result<()> {
    if input["queries"][0] == input["queries"][1] {
        return Ok(());
    }
    fn walk(v: &Value) -> Option<crate::error::FrontendError> {
        match v {
            Value::Object(m) => {
                if let Some(Value::String(t)) = m.get("type") {
                    if UNFAITHFUL.iter().any(|(n, _)| n == t) {
                        return Some(unfaithful_refusal(t));
                    }
                }
                if numeric_to_text(v) {
                    return Some(unsupported(
                        "a numeric converted to text, whose scale it shows (1.0 and 1.00 are one number \
                         and two strings) and the IR's REAL does not carry",
                    ));
                }
                m.values().find_map(walk)
            }
            Value::Array(a) => a.iter().find_map(walk),
            _ => None,
        }
    }
    input.get("queries").and_then(walk).map_or(Ok(()), Err)
}

/// Whether `v` turns a REAL, the IR's `numeric`, into text: a cast to a string type over a REAL
/// operand (a `CAST`, a named `q_cast_` for a qualified target such as `varchar(8)`, or a `qcastK`
/// of the inferring modes), or a `||` with a REAL operand, which casts it to text.
fn numeric_to_text(v: &Value) -> bool {
    let op = v.get("operator").and_then(Value::as_str).unwrap_or("");
    let operands = v.get("operand").and_then(Value::as_array).map(Vec::as_slice).unwrap_or(&[]);
    let real = |e: &Value| e.get("type").and_then(Value::as_str) == Some("REAL");
    // `get`, not slicing: an operator is any function's name, and a name need not be ASCII.
    let qcast = op.get(..5).is_some_and(|p| p.eq_ignore_ascii_case("qcast"))
        && op.get(5..).is_some_and(|k| !k.is_empty() && k.bytes().all(|b| b.is_ascii_digit()));
    let cast = op == "CAST" || op.starts_with("q_cast_") || qcast;
    let to_text = v.get("type").and_then(Value::as_str) == Some("VARCHAR");
    (cast && to_text && operands.first().is_some_and(real)) || (op == "||" && operands.iter().any(real))
}

/// The refusal for a value of the [`UNFAITHFUL`] type `name`.
pub fn unfaithful_refusal(name: &str) -> crate::error::FrontendError {
    let why = UNFAITHFUL.iter().find(|(n, _)| *n == name).map_or("", |(_, w)| w);
    unsupported(format!("a value of type {}: {why}, which no prover's equality does", name.to_lowercase()))
}

/// Map a sqlparser `DataType` to the prover's type string. Classifies on the rendered type name so
/// it stays robust across sqlparser versions, by exact name ([`scalar_class`]). Temporal types keep
/// their own names (see the module docs); floats and BINARY/BLOB/BYTEA become the opaque
/// `VARBINARY`; the [`UNFAITHFUL`] types keep their own names; unknown types pass through uppercased.
pub fn map_type(dt: &DataType) -> String {
    let s = format!("{dt}").to_uppercase();
    let base = s.split('(').next().unwrap_or(&s).trim();
    // Before the array test: an array of these compares its elements their way.
    if let Some(t) = unfaithful_type(&s) {
        return t.into();
    }
    // An array is opaque whatever its element type, and is tested first because every arm below
    // reads only the element's spelling: `int[]` would be an INTEGER and `timestamp[]` a
    // TIMESTAMP, and a scalar reading of an array value lets `||` and `= ANY` be modelled as
    // their scalar forms.
    if s.contains('[') || s.starts_with("ARRAY") || s.split_whitespace().any(|w| w == "ARRAY") {
        return "VARBINARY".into();
    }
    if let Some(t) = temporal_class(&s) {
        return t.unwrap_or("VARBINARY").into();
    }
    match scalar_class(base) {
        Some(Scalar::Int) => "INTEGER",
        Some(Scalar::Numeric) => "REAL",
        Some(Scalar::Float | Scalar::Binary) => "VARBINARY",
        Some(Scalar::Str) => "VARCHAR",
        Some(Scalar::Bool) => "BOOLEAN",
        None => base,
    }
    .to_string()
}

/// Normalise a type name written in the `declare ... function ... returns T` DSL.
///
/// The DSL names the IR's types, so `REAL`, and `DOUBLE`, the spelling the preprocessor wrote for
/// it, are the IR's exact REAL, and `VARBINARY` is the opaque type. Postgres's other names read as
/// they do in a `CREATE TABLE` ([`map_type`]): `float8` is opaque, and `char` and `citext` are
/// [`UNFAITHFUL`].
pub fn normalize_type_name(t: &str) -> String {
    let up = t.to_uppercase();
    if let Some(class) = temporal_class(&up) {
        return class.unwrap_or("VARBINARY").to_string();
    }
    if matches!(up.as_str(), "REAL" | "DOUBLE") {
        return "REAL".into();
    }
    if let Some(u) = unfaithful_type(&up) {
        return u.into();
    }
    match scalar_class(&up) {
        Some(Scalar::Int) => "INTEGER",
        Some(Scalar::Numeric) => "REAL",
        Some(Scalar::Float | Scalar::Binary) => "VARBINARY",
        Some(Scalar::Str) => "VARCHAR",
        Some(Scalar::Bool) => "BOOLEAN",
        None => return up,
    }
    .to_string()
}

/// The `type` annotation of a lowered expression Value (defaults to INTEGER if absent).
pub fn ty_of(v: &Value) -> String {
    v.get("type").and_then(|t| t.as_str()).unwrap_or("INTEGER").to_string()
}

pub fn is_num(t: &str) -> bool {
    t == "INTEGER" || t == "REAL"
}

pub fn is_builtin(t: &str) -> bool {
    matches!(t, "INTEGER" | "REAL" | "BOOLEAN" | "VARCHAR")
}

/// Common type for a comparison's two operands. A non-builtin (opaque) type such as VARBINARY wins
/// (we cast the other side to it, as Calcite does); otherwise REAL > VARCHAR > INTEGER.
// Same reasoning as `map_type`: the opaque-wins arms are separate because they answer
// different questions about which side is opaque.
#[allow(clippy::if_same_then_else)]
pub fn common_type(a: &str, b: &str) -> String {
    if a == b {
        a.into()
    } else if is_temporal(a) || is_temporal(b) {
        temporal_common(a, b)
    } else if !is_builtin(a) {
        a.into()
    } else if !is_builtin(b) {
        b.into()
    } else if a == "REAL" || b == "REAL" {
        "REAL".into()
    } else if a == "VARCHAR" || b == "VARCHAR" {
        "VARCHAR".into()
    } else {
        "INTEGER".into()
    }
}

/// Whether `v` is a string literal: a nullary VARCHAR constant other than `NULL`. That is how the
/// lowering writes one, and how both provers read one.
fn is_string_literal(v: &Value) -> bool {
    ty_of(v) == "VARCHAR"
        && !is_null_lit(v)
        && v.get("operand").and_then(|o| o.as_array()).is_some_and(|a| a.is_empty())
        && v.get("operator").is_some_and(Value::is_string)
}

/// [`common_type`] of two operands, where a string literal takes the other operand's type, as
/// Postgres resolves an `unknown` literal: in `a = '01'` over an INTEGER `a`, `'01'` is read as an
/// integer, not `a` as text. [`common_type`] alone would rank VARCHAR above INTEGER and BOOLEAN and
/// compare `a::text` with `'01'` as strings, so `a = '1' AND NOT a = '01'` would reduce to `a = '1'`.
/// For every other type of `a` the two rules already agree.
fn common_type_of(l: &Value, r: &Value) -> String {
    match (is_string_literal(l), is_string_literal(r)) {
        (true, false) => ty_of(r),
        (false, true) => ty_of(l),
        _ => common_type(&ty_of(l), &ty_of(r)),
    }
}

/// A string literal read as a value of `ty`, where `ty` is INTEGER or BOOLEAN; `None` otherwise.
///
/// Postgres reads such a literal with the type's input function, before the query runs. Where the
/// text is one the reading below understands, the literal becomes that constant: `' +01 '` is the
/// integer `1` and `'yes'` is `TRUE`. Any other text -- one Postgres rejects, which fails the query
/// before it reads a row, or one only Postgres's fuller syntax accepts, such as `'0x1F'` -- stays a
/// `CAST` of the literal, which neither prover reads as a number: each parses a cast constant with a
/// grammar narrower than the one here, so nothing reaching it gets a value.
fn read_literal(v: &Value, ty: &str) -> Option<Value> {
    if !is_string_literal(v) || !matches!(ty, "INTEGER" | "BOOLEAN") {
        return None;
    }
    let text = v["operator"].as_str().unwrap_or_default();
    let constant = match ty {
        "INTEGER" => pg_integer(text).map(|n| n.to_string()),
        _ => pg_boolean(text).map(|b| if b { "TRUE" } else { "FALSE" }.to_string()),
    };
    Some(match constant {
        Some(c) => json!({ "operator": c, "operand": [], "type": ty }),
        None => json!({ "operator": "CAST", "operand": [v], "type": ty }),
    })
}

/// The whitespace Postgres's input functions skip around a value, as far as this reading accepts it.
/// Narrower than C's `isspace`, which they use; narrower only costs a constant.
fn pg_space(c: char) -> bool {
    matches!(c, ' ' | '\t' | '\n' | '\r')
}

/// The integer Postgres's `int8in` reads from `s`, for the decimal subset of its syntax: optional
/// surrounding whitespace, an optional sign and decimal digits. `None` for anything else, Postgres's
/// own extensions (`0x1F`, `1_000`) included, and for a value past `i64`.
fn pg_integer(s: &str) -> Option<i64> {
    let t = s.trim_matches(pg_space);
    let digits = t.strip_prefix(['+', '-']).unwrap_or(t);
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    t.parse::<i64>().ok()
}

/// The boolean Postgres's `boolin` reads from `s`: after surrounding whitespace, case-insensitively,
/// a non-empty prefix of `true`, `false`, `yes` or `no`, at least two letters of `on` or `off`, or
/// `1` or `0`. `None` for anything else, which Postgres rejects.
fn pg_boolean(s: &str) -> Option<bool> {
    let t = s.trim_matches(pg_space).to_ascii_lowercase();
    let prefix_of = |word: &str| !t.is_empty() && word.starts_with(t.as_str());
    match t.as_bytes().first()? {
        b't' if prefix_of("true") => Some(true),
        b'f' if prefix_of("false") => Some(false),
        b'y' if prefix_of("yes") => Some(true),
        b'n' if prefix_of("no") => Some(false),
        b'o' if t.len() >= 2 && prefix_of("on") => Some(true),
        b'o' if t.len() >= 2 && prefix_of("off") => Some(false),
        b'1' if t.len() == 1 => Some(true),
        b'0' if t.len() == 1 => Some(false),
        _ => None,
    }
}

/// Position on Postgres's implicit-promotion chain among temporal types: DATE -> TIMESTAMP ->
/// TIMESTAMPTZ. TIME and INTERVAL are not on it; Postgres has no implicit cast between them and the
/// other three.
fn temporal_rank(t: &str) -> Option<u8> {
    match t {
        "DATE" => Some(0),
        "TIMESTAMP" => Some(1),
        "TIMESTAMPTZ" => Some(2),
        _ => None,
    }
}

/// [`common_type`] when at least one side is temporal.
///
/// Two types on the promotion chain meet at the higher one, as in Postgres (`d < ts` compares
/// `d::timestamp` with `ts`). A temporal type against a builtin wins: the builtin side is a string
/// literal, which Postgres reads as a value of the temporal type, or a NULL, which [`cast_to`]
/// relabels, or something Postgres would reject. Anything else -- TIME against a DATE, an INTERVAL
/// against a TIMESTAMP, a temporal type against an opaque one -- has no implicit cast in Postgres, and
/// the two sides meet at the opaque type, each through its own conversion. The result is never wrong
/// in the direction that matters: every crossing it asks for becomes a [`convert`] call.
fn temporal_common(a: &str, b: &str) -> String {
    match (temporal_rank(a), temporal_rank(b)) {
        (Some(x), Some(y)) => if x >= y { a } else { b }.into(),
        _ => {
            let (t, o) = if is_temporal(a) { (a, b) } else { (b, a) };
            if is_temporal(o) {
                "VARBINARY".into()
            } else if is_builtin(o) {
                t.into()
            } else {
                o.into()
            }
        }
    }
}

/// The name of the conversion from type `from` to type `to`: `q_conv_<from>_<to>`, lower-cased, with
/// the target's qualifier appended when it has one. `timestamp(0)` rounds away the fractional
/// seconds, so it is a different function from `timestamp`, and its name has to say so.
pub fn conv_name(from: &str, to: &str, qualifier: Option<&str>) -> String {
    let base = format!("q_conv_{}_{}", from.to_lowercase(), to.to_lowercase());
    match qualifier {
        Some(q) => format!("{base}_{q}"),
        None => base,
    }
}

/// Convert `v` to the type `to` across a temporal boundary: an application of the uninterpreted
/// function [`conv_name`]`(ty_of(v), to)`, never a `CAST`.
///
/// Not a `CAST` because both provers downstream erase one. QED drops a cast between two types it
/// reads as the same sort, and it reads DATE, TIME and TIMESTAMP all as its integer sort; the JVM
/// SQLSolver drops every cast. An unknown function name survives both, and it is keyed on the source
/// type as well as the target: a date's and a timestamp's conversion to text are different functions
/// even where the two values are the same integer. A NULL is relabelled rather than converted, for
/// the reason [`cast_to`] gives.
pub fn convert(v: Value, to: &str) -> Value {
    convert_qualified(v, to, None)
}

fn convert_qualified(mut v: Value, to: &str, qualifier: Option<&str>) -> Value {
    if is_null_lit(&v) {
        v["type"] = json!(to);
        return v;
    }
    json!({ "operator": conv_name(&ty_of(&v), to, qualifier), "operand": [v], "type": to })
}

/// The qualifier of an explicit temporal cast target, as a name-safe suffix: the precision of
/// `TIMESTAMP(3)` (`p3`), or the fields of `INTERVAL DAY TO SECOND`. `None` for an unqualified target.
fn cast_qualifier(spelled: &str) -> Option<String> {
    if let (Some(i), Some(j)) = (spelled.find('('), spelled.find(')')) {
        let digits: String = spelled[i + 1..j].chars().filter(|c| c.is_ascii_digit()).collect();
        return Some(format!("p{digits}"));
    }
    let rest = spelled.strip_prefix("INTERVAL")?.trim();
    (!rest.is_empty()).then(|| {
        rest.chars().map(|c| if c.is_ascii_alphanumeric() { c.to_ascii_lowercase() } else { '_' }).collect()
    })
}

/// Lower an explicit `CAST(v AS dt)`.
///
/// A cast that touches no temporal type lowers to a `CAST` node as it always has. One that does is a
/// [`convert`] call -- including a literal's, so `'2024-01-01'::date` and `'2024-01-01'::timestamp`
/// are two different terms -- except a cast to the operand's own type with no qualifier, which is the
/// identity and is dropped.
pub fn lower_cast(v: Value, dt: &DataType) -> Value {
    let target = map_type(dt);
    let from = ty_of(&v);
    if !is_temporal(&target) && !is_temporal(&from) {
        if let Some(name) = named_cast(dt, &target) {
            return json!({ "operator": name, "operand": [v], "type": target });
        }
        return cast_node(v, &target);
    }
    let qualifier = cast_qualifier(&format!("{dt}").to_uppercase());
    if from == target && qualifier.is_none() {
        return v;
    }
    convert_qualified(v, &target, qualifier.as_deref())
}

/// The function a non-temporal cast lowers to where a bare `CAST` would say too little, named after
/// the target's full spelling (`varchar(2)` -> `q_cast_varchar_2`, `int[]` -> `q_cast_int_array`);
/// `None` where a `CAST` to the IR type is exact.
///
/// A `CAST` between equal IR types is the identity to a prover, and two kinds of target map to an IR
/// type that does not say what the cast computes:
///
/// * a qualified one. A typmod is a computation the IR type does not carry: `varchar(2)` truncates
///   and `numeric(10,2)` rounds, yet both map to the type an unqualified target maps to;
/// * an opaque one. VARBINARY carries every array type and the binary ones, so `ys::int[]` over a
///   `text[]` would read as `ys`, while it parses each element.
fn named_cast(dt: &DataType, target: &str) -> Option<String> {
    let spelled = format!("{dt}").to_lowercase().replace("[]", " array");
    (spelled.contains('(') || target == "VARBINARY").then(|| {
        let safe: String = spelled.chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '_' }).collect();
        format!("q_cast_{}", safe.split('_').filter(|w| !w.is_empty()).collect::<Vec<_>>().join("_"))
    })
}

/// Whether two column types are two *different temporal types*. Where a relation-shaped construct
/// (a set operation, a `VALUES` list) would pair such columns without any comparison to hang a
/// conversion on, the values of two units would share one column.
///
/// A temporal type against a non-temporal one is not a mismatch here. That pairing is exactly what
/// the INTEGER a temporal type used to be met in the same place -- an untyped parameter in a `UNION`
/// branch is opaque, and a NULL is an INTEGER -- and the prover reads it the same way now as then.
pub fn temporal_mismatch(a: &str, b: &str) -> bool {
    a != b && is_temporal(a) && is_temporal(b)
}

/// Coerce the left operand of `x IN (subquery)` to the type of the subquery's column, which is
/// what Postgres does when that column is the higher type (`d IN (SELECT ts ..)` compares
/// `d::timestamp`, `i IN (SELECT a / 2.0 ..)` compares `i::numeric`). `Err` names the pair when the
/// conversion would have to go on the subquery's side instead, which the lowering cannot reach from
/// here.
///
/// Every pair of types, not only temporal ones: the QED prover equates the operand with the
/// subquery's column and asserts that the two have one sort, so any mismatch left here panics it. A
/// `NULL` is relabelled and a string literal read at the column's type, as in a comparison.
pub fn coerce_in_operand(x: Value, col_ty: &str) -> std::result::Result<Value, String> {
    let xt = ty_of(&x);
    if xt == col_ty {
        return Ok(x);
    }
    if is_null_lit(&x) || is_string_literal(&x) || common_type(&xt, col_ty) == col_ty {
        Ok(cast_to(x, col_ty))
    } else {
        Err(format!("{xt} compared with a subquery column of type {col_ty}"))
    }
}

/// Whether `v` is the nullary `NULL` constant.
///
/// The name alone decides it, and that is sound only because nothing else is ever a nullary node
/// named `NULL`: [`string_literal`] does not emit the string `'NULL'` as one. The type cannot decide
/// it, since a NULL is relabelled to whatever type it is coerced to.
fn is_null_lit(v: &Value) -> bool {
    v.get("operator").and_then(|o| o.as_str()) == Some("NULL")
        && v.get("operand").and_then(|o| o.as_array()).is_some_and(|a| a.is_empty())
}

/// Wrap `v` in a CAST to `ct` unless it already has that type. A crossing that involves a temporal
/// type is a [`convert`] call instead; see the module docs. A string literal cast to INTEGER or
/// BOOLEAN is read as one ([`read_literal`]).
pub fn cast_to(mut v: Value, ct: &str) -> Value {
    if ty_of(&v) == ct {
        return v;
    }
    if is_temporal(&ty_of(&v)) || is_temporal(ct) {
        return convert(v, ct);
    }
    if let Some(read) = read_literal(&v, ct) {
        return read;
    }
    // NULL is a *typed nullary constant* to the prover (`Op("NULL", [], ty)`), and it recognises a
    // value as null by comparing against the constant of that same type. Casting instead of
    // relabelling would produce `CAST(NULL_INTEGER AS REAL)`, which the prover evaluates to
    // `int_to_real(NULL_INTEGER)` — a term that is no longer *equal* to `NULL_REAL`, so it stops
    // testing as null. Relabelling is both correct (the null of type `ct` is what was meant) and
    // what keeps `IS NULL` and the aggregates' null-skipping working through a CASE branch.
    if is_null_lit(&v) {
        v["type"] = json!(ct);
        return v;
    }
    cast_node(v, ct)
}

/// Coerce two comparison operands to a common type so the prover doesn't hit a z3 sort mismatch.
/// Numeric mismatches (INTEGER vs REAL) are left for the prover's own promotion. Sound: the same
/// cast is applied deterministically on both queries, and CAST is a faithful uninterpreted coercion.
/// A string literal takes the other operand's type ([`common_type_of`]).
pub fn coerce_cmp(l: Value, r: Value) -> (Value, Value) {
    let (a, b) = (ty_of(&l), ty_of(&r));
    if a == b || (is_num(&a) && is_num(&b)) {
        return (l, r);
    }
    let ct = common_type_of(&l, &r);
    (cast_to(l, &ct), cast_to(r, &ct))
}

/// Build a comparison/equality operator (BOOLEAN result) with operand type coercion.
pub fn make_cmp(opstr: &str, l: Value, r: Value) -> Value {
    let (l, r) = coerce_cmp(l, r);
    json!({ "operator": opstr, "operand": [l, r], "type": "BOOLEAN" })
}

/// Build an arithmetic / concatenation operator with operand type coercion.
///
/// `num_ty` is the numeric result type the operator would have on numbers (INTEGER, REAL, VARCHAR
/// for `||`). An operand of a temporal type takes Postgres's own operator table instead
/// ([`temporal_arith`]).
///
/// Two rows of the numeric table are not native either, because the provers would read them as
/// exact arithmetic that Postgres does not compute, and each is an uninterpreted
/// `q_arith_<op>_<left>_<right>` function, deterministic, so both queries get the same one:
///
/// * any operator but `||` over an opaque operand, with the opaque type as its result. A float is
///   opaque, and float addition is not associative: `(0.1 + 0.2) + 0.3` is `0.6000000000000001`
///   and `0.1 + (0.2 + 0.3)` is `0.6`. So is a range, whose `+` is union. A native `+` would let a
///   prover reassociate or cancel them, and one typed INTEGER over an opaque operand is ill-sorted.
/// * `/` over a REAL operand, which is `numeric` division. It rounds to a finite scale, so
///   `x / 3.0 * 3.0` is not `x`. Both operands are converted to REAL first, as Postgres does, so
///   `a / 2.0` and `a::numeric / 2.0` are one term.
///
/// A string literal operand of anything but `||` against an INTEGER is read as an integer, as in a
/// comparison: `a + '1'` adds the integer `1`. ([`common_type`] would make it text, and every other
/// type already wins over a literal there.)
///
/// Integer `/` and `%` are never native either ([`integer_arith`]).
pub fn make_arith(opstr: &str, l: Value, r: Value, num_ty: &str) -> Value {
    let int = |v: &Value| opstr != "||" && ty_of(v) == "INTEGER";
    let (l, r) = match (is_string_literal(&l), is_string_literal(&r)) {
        (true, false) if int(&r) => (cast_to(l, "INTEGER"), r),
        (false, true) if int(&l) => (l, cast_to(r, "INTEGER")),
        _ => (l, r),
    };
    let (a, b) = (ty_of(&l), ty_of(&r));
    if is_temporal(&a) || is_temporal(&b) {
        return temporal_arith(opstr, l, r);
    }
    if opstr != "||" && !(is_builtin(&a) && is_builtin(&b)) {
        let ct = common_type(&a, &b);
        let ty = if is_builtin(&ct) { num_ty } else { &ct };
        return json!({ "operator": arith_symbol(opstr, &a, &b), "operand": [l, r], "type": ty });
    }
    if opstr == "/" && is_num(&a) && is_num(&b) && (a == "REAL" || b == "REAL") {
        return json!({
            "operator": arith_symbol("/", "REAL", "REAL"),
            "operand": [cast_to(l, "REAL"), cast_to(r, "REAL")],
            "type": "REAL"
        });
    }
    // `||` is text concatenation only over text and the other builtin scalars, and there it is
    // strict. Over anything opaque it may be something else: array `||` is not strict
    // (`'{a}' || NULL` is `{a}`), so the native operator, which provers read as strict text
    // concatenation, would make `(a || $1) IS NULL` mean `a IS NULL OR $1 IS NULL`. Such an `||` is
    // a function named after both operand types.
    if opstr == "||" && !(is_builtin(&a) && is_builtin(&b)) {
        return json!({
            "operator": format!("q_op_concat_{}_{}", a.to_lowercase(), b.to_lowercase()),
            "operand": [l, r],
            "type": "VARBINARY"
        });
    }
    if a == b || (is_num(&a) && is_num(&b)) {
        return integer_arith(opstr, l, r, num_ty);
    }
    let ct = common_type(&a, &b);
    let ty = if is_builtin(&ct) { num_ty } else { &ct };
    integer_arith(opstr, cast_to(l.clone(), &ct), cast_to(r, &ct), ty)
}

/// `l op r` of result type `ty`: the native operator, except an INTEGER `/` or `%`.
///
/// Those two are the uninterpreted functions `q_arith_div_<l>_<r>` and `q_arith_mod_<l>_<r>`, named
/// like [`temporal_arith`]'s, because a prover's integer division is not Postgres's. Postgres
/// truncates toward zero and gives `%` the sign of the dividend: `-7 / 2` is `-3` and `-7 % 2` is
/// `-1`. QED reads an INTEGER `/` as z3's `div`, which SMT-LIB defines as Euclidean division
/// (`-7 div 2` is `-4`), and has a z3 `mod` ready for `%`, which is never negative. A function is
/// sound whatever the prover knows about division: the real operator is one of its interpretations.
fn integer_arith(op: &str, l: Value, r: Value, ty: &str) -> Value {
    let name = match op {
        "/" if ty == "INTEGER" => "div",
        "%" if ty == "INTEGER" => "mod",
        _ => return json!({ "operator": op, "operand": [l, r], "type": ty }),
    };
    let operator = format!("q_arith_{name}_{}_{}", ty_of(&l).to_lowercase(), ty_of(&r).to_lowercase());
    json!({ "operator": operator, "operand": [l, r], "type": ty })
}

/// [`make_arith`] with a temporal operand: Postgres's operator table, split by whether the result
/// is linear in one unit.
///
/// Only `||` stays native: it converts its temporal side to text and concatenates.
///
/// Every other row is an uninterpreted function named after the operator and both operand types,
/// `q_arith_<op>_<left>_<right>`, with the result type Postgres gives it. That includes
/// `date ± integer` and `date - date`, which look like arithmetic on counts of days and are not:
/// Postgres leaves `infinity` unchanged under `+`, so `'infinity'::date + 1 = 'infinity'`, and raises
/// an error for `infinity - date`. It includes all interval arithmetic: an interval may count months,
/// and months have no fixed length, so `ts + iv - iv` is not `ts` (2024-01-31 plus a month, minus a
/// month, is 2024-01-29). And it includes rows Postgres rejects, such as `timestamp + integer`, which
/// come out as a function nothing else uses. A function is sound for all of these: it is
/// deterministic and both queries get the same one.
fn temporal_arith(op: &str, l: Value, r: Value) -> Value {
    let (a, b) = (ty_of(&l), ty_of(&r));
    let native = |ty: &str, l: Value, r: Value| json!({ "operator": op, "operand": [l, r], "type": ty });
    match (op, a.as_str(), b.as_str()) {
        ("||", ..) => native("VARCHAR", cast_to(l, "VARCHAR"), cast_to(r, "VARCHAR")),
        _ => {
            let ty = temporal_arith_type(op, &a, &b);
            json!({ "operator": arith_symbol(op, &a, &b), "operand": [l, r], "type": ty })
        }
    }
}

/// `q_arith_<op>_<left>_<right>`: the uninterpreted function an arithmetic operator over the types
/// `a` and `b` becomes, with each type's name reduced to lower-case letters, digits and `_`.
fn arith_symbol(op: &str, a: &str, b: &str) -> String {
    let name = match op {
        "+" => "add",
        "-" => "sub",
        "*" => "mul",
        "/" => "div",
        "%" => "mod",
        other => other,
    };
    let safe = |t: &str| -> String {
        t.to_lowercase().chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '_' }).collect()
    };
    format!("q_arith_{name}_{}_{}", safe(a), safe(b))
}

/// The result type Postgres gives a temporal operator that [`temporal_arith`] leaves uninterpreted,
/// or VARBINARY for a combination Postgres rejects.
fn temporal_arith_type(op: &str, a: &str, b: &str) -> &'static str {
    let ts = |t: &str| matches!(t, "TIMESTAMP" | "TIMESTAMPTZ");
    let num = |t: &str| is_num(t);
    match (op, a, b) {
        ("+" | "-", "TIMESTAMP", "INTERVAL") | ("+", "INTERVAL", "TIMESTAMP") => "TIMESTAMP",
        ("+" | "-", "TIMESTAMPTZ", "INTERVAL") | ("+", "INTERVAL", "TIMESTAMPTZ") => "TIMESTAMPTZ",
        ("+" | "-", "DATE", "INTEGER") | ("+", "INTEGER", "DATE") => "DATE",
        ("-", "DATE", "DATE") => "INTEGER",
        ("+" | "-", "DATE", "INTERVAL") | ("+", "INTERVAL", "DATE") => "TIMESTAMP",
        ("+", "DATE", "TIME") | ("+", "TIME", "DATE") => "TIMESTAMP",
        ("-", x, y) if ts(x) && x == y => "INTERVAL",
        ("-", "TIME", "TIME") => "INTERVAL",
        ("+" | "-", "TIME", "INTERVAL") | ("+", "INTERVAL", "TIME") => "TIME",
        ("+" | "-", "INTERVAL", "INTERVAL") => "INTERVAL",
        ("*", "INTERVAL", x) | ("*", x, "INTERVAL") if num(x) => "INTERVAL",
        ("/", "INTERVAL", x) if num(x) => "INTERVAL",
        _ => "VARBINARY",
    }
}

/// Build a `CASE` from the prover's flat `cond, result, .., else` operand list, coercing every
/// result branch to one common type.
///
/// SQL already requires the branches of a `CASE` to share a type, and the prover requires it too:
/// the node's z3 sort comes from the branches, so a `CASE` mixing sorts is ill-formed. Where our
/// inferred branch types disagree (again, usually an opaque placeholder against a builtin) we pick
/// the common type and cast each branch to it rather than letting the first branch's type stand in
/// for all of them.
///
/// The operand count is forced odd. The prover reads the two `CASE` forms off the parity — odd is
/// the searched form `[cond, body]*, else`, **even is the simple form** `input, [val, body]*, else`
/// — so handing it an even list does not mean "searched CASE with no ELSE", it silently reinterprets
/// operand 0 as a scrutinee. An absent ELSE is `NULL`, and that is what gets appended.
pub fn make_case(mut ops: Vec<Value>) -> Value {
    if ops.len().is_multiple_of(2) {
        ops.push(json!({ "operator": "NULL", "operand": [], "type": "INTEGER" }));
    }
    let n = ops.len();
    // Layout is `[cond, result]*, else`: results sit at the odd indices, the ELSE at the end.
    let mut res: Vec<usize> = (1..n).step_by(2).collect();
    if n % 2 == 1 {
        res.push(n - 1);
    }
    // A string literal branch takes the type of the others, as in a comparison ([`common_type_of`]),
    // unless every other branch is a literal or a NULL, where Postgres makes the CASE text.
    let voters: Vec<usize> = res.iter().copied().filter(|&i| !is_string_literal(&ops[i])).collect();
    let voters = if voters.iter().all(|&i| is_null_lit(&ops[i])) { res.clone() } else { voters };
    let ct = voters
        .iter()
        .map(|&i| ty_of(&ops[i]))
        .reduce(|a, b| common_type(&a, &b))
        .unwrap_or_else(|| "INTEGER".to_string());
    for &i in &res {
        ops[i] = cast_to(ops[i].take(), &ct);
    }
    // Conditions are predicates and must be boolean-sorted.
    for i in (0..n.saturating_sub(1)).step_by(2) {
        ops[i] = coerce_bool(ops[i].take());
    }
    json!({ "operator": "CASE", "operand": ops, "type": ct })
}

/// Operators whose result sort the prover derives from their operands. Relabelling one of these
/// without touching its operands would leave the node ill-sorted, so [`coerce_bool`] casts instead.
fn is_structural(op: &str) -> bool {
    matches!(op, "CASE" | "CAST" | "+" | "-" | "*" | "/" | "%" | "||")
}

/// Logical negation.
pub fn not_bool(v: Value) -> Value {
    json!({ "operator": "NOT", "operand": [v], "type": "BOOLEAN" })
}

/// In a predicate position the prover requires a BOOLEAN-typed expression (it panics otherwise).
/// A leaf operator/function used as a predicate (e.g. an undeclared `ST_DWITHIN(...)`) whose inferred
/// type isn't boolean is retyped BOOLEAN here — semantically correct (it *is* a predicate) and sound
/// (a shared uninterpreted boolean symbol, used identically on both sides). Column refs are left
/// alone (the prover types them from the schema and ignores the annotation).
pub fn coerce_bool(mut v: Value) -> Value {
    if ty_of(&v) == "BOOLEAN" {
        return v;
    }
    match v.get("operator").and_then(|o| o.as_str()) {
        // Relabelling a structural operator would contradict its own operands, so coerce it with an
        // explicit CAST — the same faithful uninterpreted coercion [`coerce_cmp`] uses.
        Some(op) if is_structural(op) => cast_to(v, "BOOLEAN"),
        Some(_) => {
            v["type"] = json!("BOOLEAN");
            v
        }
        None => v,
    }
}

/// A numeric literal as a constant: `INTEGER` when Postgres types it `integer` or `bigint`, `REAL`
/// (Postgres's `numeric`) otherwise.
///
/// Postgres types a literal with no `.` and no exponent as an integer while it fits `bigint`, and
/// every other one, `1e-5` and `9223372036854775808` included, as `numeric`. The emitted text is the
/// one Postgres prints: no `_` separators, no leading zeros, and as many fraction digits as the
/// literal's scale (`.5` is `0.5`, `1.50e1` is `15.0`, `1e1` is `10`, `1_000` is `1000`).
///
/// A `numeric` value QED cannot read exactly (see [`qed_reads_exactly`]) is not a constant at all:
/// it is `q_numeric('<text>')`, one uninterpreted function applied to the literal's text. A prover
/// knows nothing of `q_numeric` but that it is a function, so the literal's real value is one of the
/// readings it has to prove the pair under: a proof holds for the literal, and only arithmetic on it
/// is lost.
///
/// `Err` names a literal this cannot read: one not in Postgres's numeric grammar, or one whose
/// exponent is past ±1000.
pub fn number_literal(text: &str) -> std::result::Result<Value, String> {
    let n = Numeral::parse(text).ok_or_else(|| format!("numeric literal {text}"))?;
    if !n.numeric {
        if let Ok(i) = n.text.parse::<i64>() {
            return Ok(json!({ "operator": i.to_string(), "operand": [], "type": "INTEGER" }));
        }
    }
    if qed_reads_exactly(&n.int, &n.frac) {
        return Ok(json!({ "operator": n.text, "operand": [], "type": "REAL" }));
    }
    let text = json!({ "operator": n.text, "operand": [], "type": "VARCHAR" });
    Ok(json!({ "operator": "q_numeric", "operand": [text], "type": "REAL" }))
}

/// A string literal as a constant.
///
/// A string whose text lowercases to `null` is the one exception, because a nullary node with that
/// name is SQL NULL to the provers (see the module docs). It is emitted as its first
/// character concatenated with the rest: `'null'` is `'n' || 'ull'`, which is the same string in
/// any reading of `||`, and spelled with two constants neither of which is `null`. Only the
/// spelling changes, so `'null'` and `'NULL'` stay two different strings.
pub fn string_literal(s: &str) -> Value {
    let constant = |t: &str| json!({ "operator": t, "operand": [], "type": "VARCHAR" });
    if s.to_lowercase() != "null" {
        return constant(s);
    }
    let first = s.chars().next().map_or(0, char::len_utf8);
    let (head, tail) = s.split_at(first);
    json!({ "operator": "||", "operand": [constant(head), constant(tail)], "type": "VARCHAR" })
}

/// A numeric literal's text, read the way Postgres reads it.
struct Numeral {
    /// Postgres types it `numeric` rather than as an integer: it has a `.` or an exponent.
    numeric: bool,
    /// The digits before the point, without leading zeros (`"0"` if none are left).
    int: String,
    /// The digits after the point, as many as the literal's scale.
    frac: String,
    /// `int`, and `.` and `frac` if the scale is not zero: the text Postgres prints.
    text: String,
}

impl Numeral {
    /// `None` unless `text` is `digits [. digits] [e [+-] digits]` with at least one mantissa digit,
    /// after the `_` digit separators Postgres 16 admits are dropped.
    fn parse(text: &str) -> Option<Numeral> {
        let text: String = text.chars().filter(|&c| c != '_').collect();
        let (mantissa, exp) = match text.find(['e', 'E']) {
            Some(i) => (&text[..i], Some(&text[i + 1..])),
            None => (text.as_str(), None),
        };
        let numeric = mantissa.contains('.') || exp.is_some();
        let (int, frac) = mantissa.split_once('.').unwrap_or((mantissa, ""));
        let digits = |s: &str| s.bytes().all(|b| b.is_ascii_digit());
        if int.len() + frac.len() == 0 || !digits(int) || !digits(frac) {
            return None;
        }
        let exp: i64 = match exp {
            None => 0,
            Some(e) => {
                let body = e.strip_prefix(['+', '-']).unwrap_or(e);
                if body.is_empty() || !digits(body) {
                    return None;
                }
                e.parse().ok().filter(|x: &i64| x.abs() <= 1000)?
            }
        };
        // The value is `all * 10^shift`, and the scale is the number of fraction digits that leaves.
        let all = format!("{int}{frac}");
        let shift = exp - frac.len() as i64;
        let (int, frac) = if shift >= 0 {
            (format!("{all}{}", "0".repeat(shift as usize)), String::new())
        } else {
            let scale = (-shift) as usize;
            let padded = format!("{}{all}", "0".repeat((scale + 1).saturating_sub(all.len())));
            let (i, f) = padded.split_at(padded.len() - scale);
            (i.to_string(), f.to_string())
        };
        let int = match int.trim_start_matches('0') {
            "" => "0".to_string(),
            rest => rest.to_string(),
        };
        let text = if frac.is_empty() { int.clone() } else { format!("{int}.{frac}") };
        Some(Numeral { numeric, int, frac, text })
    }
}

/// Whether QED reads the decimal `int.frac` as exactly that value.
///
/// QED parses a REAL constant with `str::parse::<f32>()`, takes the `f32`'s exact value as a reduced
/// fraction `p/q`, and builds the z3 real from `p` and `q` converted to `i32` -- unwrapped, so a part
/// that does not fit panics the prover. A decimal survives that exactly when it *is* such an `f32`:
/// reduced, its denominator is a power of two no larger than `2^30`, its numerator fits an `i32`, and
/// the numerator's odd part fits the `f32`'s 24-bit significand. `sqleq-solver` reads a REAL constant
/// through `f64`, which is exact on every `f32`.
fn qed_reads_exactly(int: &str, frac: &str) -> bool {
    // `int.frac` is `d / 10^m`. Drop the factors of ten the digits share with the denominator.
    let digits = format!("{int}{frac}");
    let digits = digits.trim_start_matches('0');
    let mut m = frac.len() as u32;
    let Ok(mut d) = (if digits.is_empty() { Ok(0) } else { digits.parse::<u128>() }) else {
        return false;
    };
    if d == 0 {
        return true;
    }
    while m > 0 && d % 10 == 0 {
        d /= 10;
        m -= 1;
    }
    // What is left of `10^m = 2^m * 5^m` must cancel against `d` down to a power of two, and `d` is
    // then odd (it is a multiple of 5 and not of 10), so the fraction is reduced.
    let p = if m == 0 {
        d
    } else {
        match 5u128.checked_pow(m) {
            Some(f) if m <= 30 && d % f == 0 => d / f,
            _ => return false,
        }
    };
    let odd = p >> p.trailing_zeros();
    p <= i32::MAX as u128 && odd < 1 << 24
}

/// `CAST(v AS target)`, unless QED would read it as a value Postgres's cast does not compute.
///
/// QED evaluates a `CAST` whose operand is a constant by parsing the constant's text as the target
/// type. Text to REAL goes through the `f32` reading [`qed_reads_exactly`] describes, so
/// `'20000000.5'` would be `20000000`, and `'0.00001'` would panic the prover. Such a cast is the
/// uninterpreted conversion `q_conv_varchar_real` instead, which is what QED makes of a cast whose
/// text it cannot parse. The other crossings parse to what Postgres computes (text to INTEGER as an
/// `i64`, to BOOLEAN as `true`/`false`, a number to text as the spelling Postgres prints, which
/// [`number_literal`] emits), or fail to parse and are left uninterpreted.
fn cast_node(v: Value, target: &str) -> Value {
    let nullary = v.get("operand").and_then(Value::as_array).is_some_and(|a| a.is_empty());
    if target == "REAL" && nullary && ty_of(&v) == "VARCHAR" {
        let text = v.get("operator").and_then(Value::as_str).unwrap_or_default();
        let unsigned = text.strip_prefix(['-', '+']).unwrap_or(text);
        let exact = Numeral::parse(unsigned).is_some_and(|n| !unsigned.contains('_') && qed_reads_exactly(&n.int, &n.frac));
        if !exact && !is_null_lit(&v) {
            return json!({ "operator": conv_name("VARCHAR", "REAL", None), "operand": [v], "type": "REAL" });
        }
    }
    json!({ "operator": "CAST", "operand": [v], "type": target })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn col(i: u64, ty: &str) -> Value {
        json!({ "column": i, "type": ty })
    }

    /// `l op r` as `make_cmp` builds it once the operands already share a type.
    fn plain(op: &str, l: Value, r: Value) -> Value {
        json!({ "operator": op, "operand": [l, r], "type": "BOOLEAN" })
    }

    #[test]
    fn a_truncated_timestamp_is_compared_as_it_stands() {
        // `x::date = e` is not restated as a range on `x`: at `x = e = 'infinity'` the truncation
        // holds and `x < (e + 1)::timestamp` does not, since `'infinity'::date + 1` is `infinity`.
        let t = convert(col(0, "TIMESTAMP"), "DATE");
        assert_eq!(make_cmp("=", t.clone(), col(1, "DATE")), plain("=", t, col(1, "DATE")));
    }

    #[test]
    fn date_arithmetic_is_a_function_not_integer_addition() {
        let one = json!({ "operator": "1", "operand": [], "type": "INTEGER" });
        let add = make_arith("+", col(0, "DATE"), one.clone(), "INTEGER");
        assert_eq!((add["operator"].as_str(), add["type"].as_str()), (Some("q_arith_add_date_integer"), Some("DATE")));
        let rev = make_arith("+", one, col(0, "DATE"), "INTEGER");
        assert_eq!((rev["operator"].as_str(), rev["type"].as_str()), (Some("q_arith_add_integer_date"), Some("DATE")));
        let diff = make_arith("-", col(0, "DATE"), col(1, "DATE"), "INTEGER");
        assert_eq!((diff["operator"].as_str(), diff["type"].as_str()), (Some("q_arith_sub_date_date"), Some("INTEGER")));
        // Concatenation is still native, over the converted text.
        let cat = make_arith("||", col(0, "DATE"), col(2, "VARCHAR"), "VARCHAR");
        assert_eq!(cat["operator"], "||");
    }

    #[test]
    fn crossings_are_conversions_and_the_same_type_is_not() {
        assert_eq!(cast_to(col(0, "DATE"), "DATE"), col(0, "DATE"));
        let c = cast_to(col(0, "DATE"), "TIMESTAMP");
        assert_eq!(c["operator"], "q_conv_date_timestamp");
        assert_eq!(c["type"], "TIMESTAMP");
        // A NULL is relabelled, not converted.
        let null = json!({ "operator": "NULL", "operand": [], "type": "INTEGER" });
        assert_eq!(cast_to(null, "DATE")["operator"], "NULL");
        // The promotion chain, and the off-chain meet at the opaque type.
        assert_eq!(common_type("DATE", "TIMESTAMP"), "TIMESTAMP");
        assert_eq!(common_type("TIMESTAMPTZ", "DATE"), "TIMESTAMPTZ");
        assert_eq!(common_type("TIME", "DATE"), "VARBINARY");
        assert_eq!(common_type("VARCHAR", "DATE"), "DATE");
    }
}
