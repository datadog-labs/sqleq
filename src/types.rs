//! Type mapping and coercion helpers.
//!
//! The prover's `DataType` is one of INTEGER / REAL / BOOLEAN / VARCHAR or a `Custom(name)` (e.g.
//! VARBINARY, geometry types). We render types as the uppercase strings the prover deserializes.

use serde_json::{json, Value};
use sqlparser::ast::DataType;

/// Map a sqlparser `DataType` to the prover's type string. Classifies on the rendered type name so
/// it stays robust across sqlparser versions. DATE/TIME/TIMESTAMP map to INTEGER (the prover aliases
/// them); BINARY/BLOB/BYTEA become the opaque `VARBINARY`; unknown types pass through uppercased.
// The arms below are kept apart on purpose: each names a distinct source class, and two of
// them happening to land on INTEGER is a fact about the prover's type set, not a redundancy.
#[allow(clippy::if_same_then_else)]
pub fn map_type(dt: &DataType) -> String {
    let s = format!("{dt}").to_uppercase();
    let base = s.split('(').next().unwrap_or(&s).trim();
    if base.contains("CHAR") || base.contains("TEXT") || base.contains("STRING") || base.contains("CLOB") {
        "VARCHAR".into()
    } else if base.contains("BINARY") || base == "BLOB" || base == "BYTEA" {
        "VARBINARY".into()
    } else if base.starts_with("BOOL") {
        "BOOLEAN".into()
    } else if base == "DATE" || base.starts_with("TIME") || base == "DATETIME" {
        "INTEGER".into()
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
    match t.to_uppercase().as_str() {
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

/// Whether `v` is the nullary `NULL` constant.
fn is_null_lit(v: &Value) -> bool {
    v.get("operator").and_then(|o| o.as_str()) == Some("NULL")
        && v.get("operand").and_then(|o| o.as_array()).is_some_and(|a| a.is_empty())
}

/// Wrap `v` in a CAST to `ct` unless it already has that type.
pub fn cast_to(mut v: Value, ct: &str) -> Value {
    if ty_of(&v) == ct {
        return v;
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
/// for `||`). That rule is right for numbers but wrong once an operand is opaque: the preprocessor
/// renders TIMESTAMP and friends as VARBINARY, so `ts + n` would otherwise emit an INTEGER-typed
/// `+` over mismatched operand sorts and the prover builds an ill-sorted z3 term from it. Coercing
/// both sides to their common type — and taking that type as the result when it is opaque — keeps
/// the term well-sorted. Sound for the same reason as [`coerce_cmp`]: the cast is deterministic, so
/// both queries get it identically, and `+` over an opaque type is uninterpreted either way.
pub fn make_arith(opstr: &str, l: Value, r: Value, num_ty: &str) -> Value {
    let (a, b) = (ty_of(&l), ty_of(&r));
    if a == b || (is_num(&a) && is_num(&b)) {
        return json!({ "operator": opstr, "operand": [l, r], "type": num_ty });
    }
    let ct = common_type(&a, &b);
    let ty = if is_builtin(&ct) { num_ty } else { &ct };
    json!({ "operator": opstr, "operand": [cast_to(l.clone(), &ct), cast_to(r, &ct)], "type": ty })
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
