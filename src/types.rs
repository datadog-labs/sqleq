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

use serde_json::{json, Value};
use sqlparser::ast::DataType;

/// The temporal types this module keeps apart, as the type strings the rest of the frontend uses.
pub const TEMPORAL: &[&str] = &["DATE", "TIME", "TIMESTAMP", "TIMESTAMPTZ", "INTERVAL"];

/// Whether `t` is one of the [`TEMPORAL`] types.
pub fn is_temporal(t: &str) -> bool {
    TEMPORAL.contains(&t)
}

/// The spelling a type leaves the frontend with. Only one differs: TIMESTAMPTZ is emitted as
/// TIMESTAMP.
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

/// Map a sqlparser `DataType` to the prover's type string. Classifies on the rendered type name so
/// it stays robust across sqlparser versions. Temporal types keep their own names (see the module
/// docs); BINARY/BLOB/BYTEA become the opaque `VARBINARY`; unknown types pass through uppercased.
// The arms below are kept apart on purpose: each names a distinct source class, and two of
// them happening to land on the same type is a fact about the prover's type set, not a redundancy.
#[allow(clippy::if_same_then_else)]
pub fn map_type(dt: &DataType) -> String {
    let s = format!("{dt}").to_uppercase();
    let base = s.split('(').next().unwrap_or(&s).trim();
    // An array is opaque whatever its element type, and is tested first because every arm below
    // reads only the element's spelling: `int[]` would be an INTEGER and `timestamp[]` a
    // TIMESTAMP, and a scalar reading of an array value lets `||` and `= ANY` be modelled as
    // their scalar forms.
    if s.contains('[') || s.starts_with("ARRAY") || s.split_whitespace().any(|w| w == "ARRAY") {
        return "VARBINARY".into();
    }
    if let Some(t) = temporal_class(&s) {
        // Before the `INT` arm below, which INTERVAL would otherwise reach through its spelling.
        t.unwrap_or("VARBINARY").into()
    } else if base.contains("CHAR") || base.contains("TEXT") || base.contains("STRING") || base.contains("CLOB") {
        "VARCHAR".into()
    } else if base.contains("BINARY") || base == "BLOB" || base == "BYTEA" {
        "VARBINARY".into()
    } else if base.starts_with("BOOL") {
        "BOOLEAN".into()
    } else if base.contains("INT") {
        "INTEGER".into()
    } else if base.contains("REAL") || base.contains("FLOAT") || base.contains("DOUBLE")
        || base.contains("DEC") || base.contains("NUMERIC")
    {
        "REAL".into()
    } else {
        base.to_string()
    }
}

/// Normalise a type name written in the `declare ... function ... returns T` DSL.
pub fn normalize_type_name(t: &str) -> String {
    let up = t.to_uppercase();
    if let Some(class) = temporal_class(&up) {
        return class.unwrap_or("VARBINARY").to_string();
    }
    match up.as_str() {
        "INT" | "INTEGER" | "SMALLINT" | "BIGINT" | "TINYINT" => "INTEGER",
        "VARCHAR" | "CHAR" | "TEXT" | "STRING" => "VARCHAR",
        "BOOL" | "BOOLEAN" => "BOOLEAN",
        "FLOAT" | "DOUBLE" | "REAL" | "DECIMAL" | "NUMERIC" => "REAL",
        other => return other.to_string(),
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
        return json!({ "operator": "CAST", "operand": [v], "type": target });
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
/// (a set operation, a `VALUES` list, an `IN (subquery)`) would pair such columns without any
/// comparison to hang a conversion on, the values of two units would share one column.
///
/// A temporal type against a non-temporal one is not a mismatch here. That pairing is exactly what
/// the INTEGER a temporal type used to be met in the same place -- an untyped parameter in a `UNION`
/// branch is opaque, and a NULL is an INTEGER -- and the prover reads it the same way now as then.
pub fn temporal_mismatch(a: &str, b: &str) -> bool {
    a != b && is_temporal(a) && is_temporal(b)
}

/// Coerce the left operand of `x IN (subquery)` to the type of the subquery's column, which is
/// what Postgres does when that column is the higher type (`d IN (SELECT ts ..)` compares
/// `d::timestamp`). `Err` names the pair when the conversion would have to go on the subquery's
/// side instead, which the lowering cannot reach from here.
pub fn coerce_in_operand(x: Value, col_ty: &str) -> std::result::Result<Value, String> {
    let xt = ty_of(&x);
    if !temporal_mismatch(&xt, col_ty) || is_null_lit(&x) {
        return Ok(x);
    }
    if common_type(&xt, col_ty) == col_ty {
        Ok(cast_to(x, col_ty))
    } else {
        Err(format!("{xt} compared with a subquery column of type {col_ty}"))
    }
}

/// Whether `v` is the nullary `NULL` constant.
fn is_null_lit(v: &Value) -> bool {
    v.get("operator").and_then(|o| o.as_str()) == Some("NULL")
        && v.get("operand").and_then(|o| o.as_array()).is_some_and(|a| a.is_empty())
}

/// Wrap `v` in a CAST to `ct` unless it already has that type. A crossing that involves a temporal
/// type is a [`convert`] call instead; see the module docs.
pub fn cast_to(mut v: Value, ct: &str) -> Value {
    if ty_of(&v) == ct {
        return v;
    }
    if is_temporal(&ty_of(&v)) || is_temporal(ct) {
        return convert(v, ct);
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
    json!({ "operator": "CAST", "operand": [v], "type": ct })
}

/// Coerce two comparison operands to a common type so the prover doesn't hit a z3 sort mismatch.
/// Numeric mismatches (INTEGER vs REAL) are left for the prover's own promotion. Sound: the same
/// cast is applied deterministically on both queries, and CAST is a faithful uninterpreted coercion.
pub fn coerce_cmp(l: Value, r: Value) -> (Value, Value) {
    let (a, b) = (ty_of(&l), ty_of(&r));
    if a == b || (is_num(&a) && is_num(&b)) {
        return (l, r);
    }
    let ct = common_type(&a, &b);
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
/// ([`temporal_arith`]). Past that, the numeric rule is still wrong once an operand is opaque: an
/// INTEGER-typed `+` over an opaque operand is ill-sorted, and the prover builds a broken z3 term
/// from it. Coercing both sides to their common type — and taking that type as the result when it
/// is opaque — keeps the term well-sorted. Sound for the same reason as [`coerce_cmp`]: the cast is
/// deterministic, so both queries get it identically, and `+` over an opaque type is uninterpreted
/// either way.
///
/// Integer `/` and `%` are never native ([`integer_arith`]).
pub fn make_arith(opstr: &str, l: Value, r: Value, num_ty: &str) -> Value {
    let (a, b) = (ty_of(&l), ty_of(&r));
    if is_temporal(&a) || is_temporal(&b) {
        return temporal_arith(opstr, l, r);
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
            let name = match op {
                "+" => "add",
                "-" => "sub",
                "*" => "mul",
                "/" => "div",
                "%" => "mod",
                other => other,
            };
            json!({
                "operator": format!("q_arith_{name}_{}_{}", a.to_lowercase(), b.to_lowercase()),
                "operand": [l, r],
                "type": ty
            })
        }
    }
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
    if ops.len() % 2 == 0 {
        ops.push(json!({ "operator": "NULL", "operand": [], "type": "INTEGER" }));
    }
    let n = ops.len();
    // Layout is `[cond, result]*, else`: results sit at the odd indices, the ELSE at the end.
    let mut res: Vec<usize> = (1..n).step_by(2).collect();
    if n % 2 == 1 {
        res.push(n - 1);
    }
    let ct = res
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
