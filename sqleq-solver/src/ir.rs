// Unless explicitly stated otherwise all files in this repository are licensed under the
// Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/).
// Copyright 2026-Present Datadog, Inc.

//! Typed IR mirroring the `Input` JSON wire shape the JVM bridge (`tools/sqlsolver/IrToRel.java` in
//! the public repo) consumes today, re-derived directly from three sources: `IrToRel.java` itself
//! (read in full), `src/lib.rs`'s `emit()` (the emitting side's top-level constructor), and
//! `src/verify.rs`'s `check_levels` (a second, independent reader of the same shape). All three agree.
//!
//! ## Two-stage design, deliberately
//!
//! `IrToRel.java` does shape validation and scope-dependent column/aggregate resolution in the same
//! recursive walk, because it builds a Calcite `RelNode`/`RexNode` pair where both kinds of check
//! happen to live together. This port skips Calcite entirely, so nothing forces that fusion here,
//! and keeping the stages apart is more testable:
//!
//! - **This module (parse/shape stage)**: is a JSON value shaped like a `Relation`/`Expr` at all --
//!   right tag, right arity, right JSON types? These checks need no context beyond the node itself
//!   (and, for `scan`, the schema count).
//! - **The translation stage** (`translate.rs`): column references
//!   are only correlated/in-range/well-typed *relative to an enclosing scope*, and an aggregate
//!   function name is only supported *relative to what the target prover accepts* -- both of which
//!   need the same `base`/row-type threading `IrToRel.rel()`/`expr()` does. `TranslateError`
//!   carries the variants those checks use (`Correlated`, `ColumnOutOfRange`, `Aggregate`,
//!   `GroupKeyNonRef`, `AggArgNonRef`) so both stages report through one enum, matching
//!   `IrToRel.java`'s single `Refused` exception spanning both jobs.
//!
//! Java also refuses `scan-unknown-table`, when its name-based Calcite catalog fails to resolve a
//! table. We index tables by position straight out of `Input.schemas`, so there is no name lookup
//! to fail, and only `scan-out-of-range` exists here.
//!
//! `Input.help` (a per-query Calcite `explain()` string) and `Schema.guaranteed` (an integrity-
//! constraint list *our* frontend always emits empty, and `IrToRel.java` never reads at all -- see
//! `IrToRel.type()`/`rel()`, neither of which touches either key) are both dead weight from this
//! bridge's perspective and are intentionally not modeled: parsing ignores them rather than
//! round-tripping fields nothing here or on the Java side consumes.

use serde_json::Value;

/// The closed type vocabulary every column and literal is drawn from (`IrToRel.type()`/
/// `literal()`, cross-checked against the emitting side's `types.rs::map_type`): the five builtin
/// types and the four temporal ones. `VARBINARY` is `IrToRel`'s own opaque catch-all, where
/// BINARY/BLOB/BYTEA and every unmodelled type land.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Type {
    Integer,
    Real,
    Boolean,
    Varchar,
    Varbinary,
    /// The temporal types, each an integer in its own unit (days, microseconds since midnight,
    /// microseconds). The frontend never lets two of them meet except through a named conversion
    /// (`q_conv_<from>_<to>`), and emits TIMESTAMPTZ as TIMESTAMP. INTERVAL is opaque: it may
    /// count months, and it only ever reaches arithmetic through uninterpreted `q_arith_*` calls.
    Date,
    Time,
    Timestamp,
    Interval,
}

impl Type {
    pub fn parse(s: &str) -> Result<Type, TranslateError> {
        Ok(match s {
            "INTEGER" => Type::Integer,
            "REAL" => Type::Real,
            "BOOLEAN" => Type::Boolean,
            "VARCHAR" => Type::Varchar,
            "VARBINARY" => Type::Varbinary,
            "DATE" => Type::Date,
            "TIME" => Type::Time,
            "TIMESTAMP" => Type::Timestamp,
            "INTERVAL" => Type::Interval,
            other => return Err(TranslateError::UnknownType(other.to_string())),
        })
    }

    pub fn name(self) -> &'static str {
        match self {
            Type::Integer => "INTEGER",
            Type::Real => "REAL",
            Type::Boolean => "BOOLEAN",
            Type::Varchar => "VARCHAR",
            Type::Varbinary => "VARBINARY",
            Type::Date => "DATE",
            Type::Time => "TIME",
            Type::Timestamp => "TIMESTAMP",
            Type::Interval => "INTERVAL",
        }
    }
}

/// The refusal taxonomy `IrToRel.java` throws as its one `Refused(String)` exception, one variant
/// per distinct reason string, plus a handful of Rust-only additions noted where they occur.
/// `Display` reproduces the exact strings `IrToRel.java` produces, so the two drivers' refusals can be
/// compared row by row on the same inputs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TranslateError {
    RelNotObject,
    ScanOutOfRange,
    DistinctNotARelation,
    /// `rel:<tag>` -- an object relation whose one key isn't one this bridge understands.
    UnknownRelation(String),
    /// `<kind>-arity` -- a `union`/`except`/`intersect` array without exactly two branches.
    SetOpArity(String),
    ValuesNonLiteral,
    CollationShape,
    /// `join-kind:<k>` -- anything but INNER/LEFT/RIGHT/FULL (we never emit SEMI/ANTI).
    JoinKind(String),
    /// `aggregate:<op>` -- `op` already stripped of a `#`-suffix, matching `IrToRel.agg()`'s
    /// `op.split("#")[0]` (our own opaque aggregate carriers, e.g. `DISTINCT_ON#3`, carry one).
    Aggregate(String),
    GroupKeyNonRef,
    AggArgNonRef,
    ExprNotObject,
    Correlated,
    ColumnOutOfRange,
    CastArity,
    /// `subquery:<op>` -- anything but EXISTS/$SCALAR_QUERY/IN under a `query` key.
    Subquery(String),
    /// `literal:<tyName>` -- an INTEGER/REAL literal whose value string isn't numeric.
    Literal(String),
    /// Not part of `IrToRel`'s taxonomy: it silently falls back to `SqlTypeName.ANY` for an
    /// unrecognized type string instead of refusing. Our own frontend emits only the types [`Type`]
    /// names, so this is unreachable on its output; refusing loudly here rather than inventing an
    /// ANY variant matches this codebase's general refuse-rather-than-silently-misinterpret posture.
    UnknownType(String),
    /// Not part of `IrToRel`'s taxonomy: Java NPEs on a field that's absent where it expects one
    /// (missing `source`, a non-array `collation`, etc.) since it never checks before dereferencing.
    /// Rust has no such fallback, so a shape defect Java would crash on is reported here instead --
    /// harmless on real input (which is always well-formed), and strictly safer than a panic.
    MalformedShape(String),
    /// Translation-stage-only, not part of `IrToRel`'s taxonomy: a `Sort` with an `offset`/`limit`
    /// present. Bare `ORDER BY` (no offset/limit) is a no-op under bag semantics and passes through;
    /// real LIMIT/OFFSET semantics (SQLSolver's `OrderbySupport`) is not ported.
    UnsupportedSort,
    /// Translation-stage-only: an aggregate call with `distinct: true`. SQLSolver's own translator
    /// handles this via a second nested existential sum; this phase deliberately doesn't port that
    /// and refuses instead, deferring it rather than half-building it.
    DistinctAggregateUnsupported,
    /// Translation-stage-only: a scalar subquery (`$SCALAR_QUERY`) whose inner relation doesn't have
    /// exactly one output column.
    ScalarSubqueryArity,
    /// Translation-stage-only: `dedup-not-identity:<type>` -- a `DISTINCT`, `GROUP BY` key,
    /// `UNION`, `INTERSECT` or `EXCEPT` over a column whose `=` is not identity of the values (see
    /// `translate::eq_is_identity`). Postgres keeps one row per class of `=`-equal values, and which
    /// of them it keeps is not modelled; `unresolved` names a set-operation column whose two branches
    /// have different IR types.
    DedupNotIdentity(String),
    /// Not part of `IrToRel`'s taxonomy: a plan nested deeper than [`MAX_DEPTH`]. Every stage
    /// recurses over the plan, so a bound is what keeps a deep plan a refusal instead of a stack
    /// overflow, which aborts the whole process.
    TooDeep,
}

impl std::fmt::Display for TranslateError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TranslateError::RelNotObject => write!(f, "rel-not-object"),
            TranslateError::ScanOutOfRange => write!(f, "scan-out-of-range"),
            TranslateError::DistinctNotARelation => write!(f, "distinct-not-a-relation"),
            TranslateError::UnknownRelation(tag) => write!(f, "rel:{tag}"),
            TranslateError::SetOpArity(kind) => write!(f, "{kind}-arity"),
            TranslateError::ValuesNonLiteral => write!(f, "values-nonliteral"),
            TranslateError::CollationShape => write!(f, "collation-shape"),
            TranslateError::JoinKind(k) => write!(f, "join-kind:{k}"),
            TranslateError::Aggregate(op) => write!(f, "aggregate:{}", op.split('#').next().unwrap_or(op)),
            TranslateError::GroupKeyNonRef => write!(f, "group-key-nonref"),
            TranslateError::AggArgNonRef => write!(f, "agg-arg-nonref"),
            TranslateError::ExprNotObject => write!(f, "expr-not-object"),
            TranslateError::Correlated => write!(f, "correlated"),
            TranslateError::ColumnOutOfRange => write!(f, "column-out-of-range"),
            TranslateError::CastArity => write!(f, "cast-arity"),
            TranslateError::Subquery(op) => write!(f, "subquery:{op}"),
            TranslateError::Literal(ty) => write!(f, "literal:{ty}"),
            TranslateError::UnknownType(t) => write!(f, "unknown-type:{t}"),
            TranslateError::MalformedShape(msg) => write!(f, "malformed-shape:{msg}"),
            TranslateError::UnsupportedSort => write!(f, "unsupported-sort"),
            TranslateError::DistinctAggregateUnsupported => write!(f, "aggregate-distinct-unsupported"),
            TranslateError::ScalarSubqueryArity => write!(f, "scalar-subquery-arity"),
            TranslateError::DedupNotIdentity(ty) => write!(f, "dedup-not-identity:{ty}"),
            TranslateError::TooDeep => write!(f, "nesting-too-deep"),
        }
    }
}

impl std::error::Error for TranslateError {}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JoinKind {
    Inner,
    Left,
    Right,
    Full,
}

impl JoinKind {
    fn parse(k: &str) -> Result<JoinKind, TranslateError> {
        Ok(match k {
            "INNER" => JoinKind::Inner,
            "LEFT" => JoinKind::Left,
            "RIGHT" => JoinKind::Right,
            "FULL" => JoinKind::Full,
            // SEMI/ANTI are the two SQLSolver's own translator doesn't accept either; we never emit
            // them, so this is a real refusal path but not one our own frontend's output reaches.
            other => return Err(TranslateError::JoinKind(other.to_string())),
        })
    }
}

/// Checks one `[columnIndex, type, "ASCENDING NULLS LAST"]` sort key's shape, which is all that is
/// done with it: a sort is translated only when it has no `offset` or `limit`, and then its order
/// is erased (bag semantics), so nothing reads the key itself.
fn check_collation(v: &Value) -> Result<(), TranslateError> {
    let arr = v.as_array().filter(|a| a.len() >= 3).ok_or(TranslateError::CollationShape)?;
    arr[0].as_u64().ok_or(TranslateError::CollationShape)?;
    Type::parse(arr[1].as_str().ok_or(TranslateError::CollationShape)?)?;
    arr[2].as_str().ok_or(TranslateError::CollationShape)?;
    Ok(())
}

/// One `GROUP BY`/global aggregate function call. `distinct` and `operand` are both optional on the
/// wire (`f.path("distinct")`/`f.has("operand")` in `IrToRel.agg()`), defaulting to `false`/`[]`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AggCall {
    pub operator: String,
    pub ty: Type,
    pub distinct: bool,
    pub operand: Vec<Expr>,
}

impl AggCall {
    fn parse(v: &Value, schema_count: usize) -> Result<AggCall, TranslateError> {
        let operator = field(v, "operator")?
            .as_str()
            .ok_or_else(|| TranslateError::MalformedShape("aggregate call operator not a string".into()))?
            .to_string();
        let ty = Type::parse(v.get("type").and_then(Value::as_str).unwrap_or(""))?;
        let distinct = v.get("distinct").and_then(Value::as_bool).unwrap_or(false);
        let operand = match v.get("operand").and_then(Value::as_array) {
            None => Vec::new(),
            Some(a) => a.iter().map(|e| Expr::parse(e, schema_count)).collect::<Result<_, _>>()?,
        };
        Ok(AggCall { operator, ty, distinct, operand })
    }
}

/// A relation node: exactly the 11 tags `IrToRel.rel()` dispatches on (`scan`/`distinct`/`filter`/
/// `project`/`join`/`group`/`sort`/`values`/`union`/`except`/`intersect`). A bare `"singleton"`
/// string relation (a `FROM`-less query) is *not* a variant here, deliberately: `IrToRel.rel()`
/// requires `r.isObject()` unconditionally and refuses a bare string as `rel-not-object`, so this
/// bridge already can't reach a FROM-less query and this port must refuse it the same way rather
/// than add a case Java doesn't have.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Relation {
    Scan(usize),
    Distinct(Box<Relation>),
    Filter { source: Box<Relation>, condition: Expr },
    Project { source: Box<Relation>, target: Vec<Expr> },
    Join { left: Box<Relation>, right: Box<Relation>, kind: JoinKind, condition: Expr },
    Group { source: Box<Relation>, keys: Vec<Expr>, function: Vec<AggCall> },
    /// A sort's keys are checked when parsed (see `check_collation`) and not kept.
    Sort { source: Box<Relation>, offset: Option<Expr>, limit: Option<Expr> },
    Values { schema: Vec<Type>, content: Vec<Vec<Expr>> },
    Union([Box<Relation>; 2]),
    Except([Box<Relation>; 2]),
    Intersect([Box<Relation>; 2]),
}

impl Relation {
    /// `schema_count` is `Input.schemas.len()`, the only context a `scan` needs at this stage --
    /// everything scope-dependent (`correlated`, aggregate-name support, ordinal resolution of group
    /// keys/agg args) is the translation stage's job, not this one's (see the module doc).
    pub fn parse(v: &Value, schema_count: usize) -> Result<Relation, TranslateError> {
        let o = v.as_object().ok_or(TranslateError::RelNotObject)?;

        if let Some(x) = o.get("scan") {
            let i = x.as_u64().ok_or(TranslateError::ScanOutOfRange)? as usize;
            if i >= schema_count {
                return Err(TranslateError::ScanOutOfRange);
            }
            return Ok(Relation::Scan(i));
        }
        if let Some(x) = o.get("distinct") {
            if !x.is_object() {
                return Err(TranslateError::DistinctNotARelation);
            }
            let source = Relation::parse(x, schema_count)?;
            return Ok(Relation::Distinct(Box::new(source)));
        }
        if let Some(x) = o.get("filter") {
            let source = Relation::parse(field(x, "source")?, schema_count)?;
            let condition = Expr::parse(field(x, "condition")?, schema_count)?;
            return Ok(Relation::Filter { source: Box::new(source), condition });
        }
        if let Some(x) = o.get("project") {
            let source = Relation::parse(field(x, "source")?, schema_count)?;
            let target = array(field(x, "target")?)?
                .iter()
                .map(|e| Expr::parse(e, schema_count))
                .collect::<Result<_, _>>()?;
            return Ok(Relation::Project { source: Box::new(source), target });
        }
        if let Some(x) = o.get("join") {
            let left = Relation::parse(field(x, "left")?, schema_count)?;
            let right = Relation::parse(field(x, "right")?, schema_count)?;
            let kind = JoinKind::parse(field(x, "kind")?.as_str().unwrap_or(""))?;
            let condition = Expr::parse(field(x, "condition")?, schema_count)?;
            return Ok(Relation::Join { left: Box::new(left), right: Box::new(right), kind, condition });
        }
        if let Some(x) = o.get("group") {
            let source = Relation::parse(field(x, "source")?, schema_count)?;
            let keys = array(field(x, "keys")?)?
                .iter()
                .map(|e| Expr::parse(e, schema_count))
                .collect::<Result<_, _>>()?;
            let function = array(field(x, "function")?)?
                .iter()
                .map(|f| AggCall::parse(f, schema_count))
                .collect::<Result<_, _>>()?;
            return Ok(Relation::Group { source: Box::new(source), keys, function });
        }
        if let Some(x) = o.get("sort") {
            let source = Relation::parse(field(x, "source")?, schema_count)?;
            array(field(x, "collation")?)?.iter().try_for_each(check_collation)?;
            let offset = opt_expr(x, "offset", schema_count)?;
            let limit = opt_expr(x, "limit", schema_count)?;
            return Ok(Relation::Sort { source: Box::new(source), offset, limit });
        }
        if let Some(x) = o.get("values") {
            let schema = array(field(x, "schema")?)?
                .iter()
                .map(|t| {
                    t.as_str()
                        .ok_or_else(|| TranslateError::MalformedShape("values schema entry not a string".into()))
                        .and_then(Type::parse)
                })
                .collect::<Result<_, _>>()?;
            let content = array(field(x, "content")?)?
                .iter()
                .map(|row| {
                    array(row)?
                        .iter()
                        .map(|cell| match Expr::parse(cell, schema_count)? {
                            // `LogicalValues` holds literals only (`IrToRel.values()`): a computed
                            // cell would need a `Project` over a one-row values, which our frontend
                            // never emits.
                            lit @ Expr::Literal { .. } => Ok(lit),
                            _ => Err(TranslateError::ValuesNonLiteral),
                        })
                        .collect::<Result<_, _>>()
                })
                .collect::<Result<_, _>>()?;
            return Ok(Relation::Values { schema, content });
        }
        if let Some(x) = o.get("union") {
            return set_op(x, schema_count, "union").map(Relation::Union);
        }
        if let Some(x) = o.get("except") {
            return set_op(x, schema_count, "except").map(Relation::Except);
        }
        if let Some(x) = o.get("intersect") {
            return set_op(x, schema_count, "intersect").map(Relation::Intersect);
        }

        let tag = o.keys().next().cloned().unwrap_or_default();
        Err(TranslateError::UnknownRelation(tag))
    }

    /// The IR type of each output column, `schemas` giving the scanned tables'. A set operation
    /// whose two branches disagree on a column's type leaves that column `None`: Postgres resolves
    /// such a column to a common type the IR does not record.
    pub fn output_types(&self, schemas: &[Schema]) -> Vec<Option<Type>> {
        match self {
            Relation::Scan(i) => schemas.get(*i).map(|s| s.types.iter().copied().map(Some).collect()).unwrap_or_default(),
            Relation::Distinct(source) | Relation::Filter { source, .. } | Relation::Sort { source, .. } => {
                source.output_types(schemas)
            }
            Relation::Project { target, .. } => target.iter().map(|e| Some(e.ty())).collect(),
            Relation::Join { left, right, .. } => {
                let mut types = left.output_types(schemas);
                types.extend(right.output_types(schemas));
                types
            }
            Relation::Group { keys, function, .. } => {
                keys.iter().map(|k| Some(k.ty())).chain(function.iter().map(|f| Some(f.ty))).collect()
            }
            Relation::Values { schema, .. } => schema.iter().copied().map(Some).collect(),
            Relation::Union(sides) | Relation::Except(sides) | Relation::Intersect(sides) => {
                let (l, r) = (sides[0].output_types(schemas), sides[1].output_types(schemas));
                l.iter().zip(r.iter()).map(|(a, b)| if a == b { *a } else { None }).collect()
            }
        }
    }
}

fn set_op(v: &Value, schema_count: usize, kind: &str) -> Result<[Box<Relation>; 2], TranslateError> {
    let arr = v.as_array().filter(|a| a.len() == 2).ok_or_else(|| TranslateError::SetOpArity(kind.to_string()))?;
    let a = Relation::parse(&arr[0], schema_count)?;
    let b = Relation::parse(&arr[1], schema_count)?;
    Ok([Box::new(a), Box::new(b)])
}

/// An expression node: either `{"column": n, "type": T}`, a subquery (`{"operator", "type", "query",
/// "operand"?}`), a literal (an operator call with no operands -- the operator string *is* the
/// value, per `IrToRel.literal()`), a `CAST`, or a general call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Expr {
    Column { index: u32, ty: Type },
    /// The operator string doubles as the literal's value; `"NULL"` is the typed null constant.
    /// Relabeling the type on a null (rather than wrapping it in a `Cast`) is what this crate must
    /// preserve going forward -- a real `Cast` node would make it a *computed* value no longer
    /// recognized as null by null-skipping semantics downstream.
    Literal { value: String, ty: Type },
    Call { operator: String, ty: Type, operand: Vec<Expr> },
    Cast { ty: Type, operand: Box<Expr> },
    /// `operand` is only ever non-empty for `IN` (the left-hand tuple); EXISTS/$SCALAR_QUERY carry
    /// none.
    Subquery { operator: String, ty: Type, operand: Vec<Expr>, query: Box<Relation> },
}

impl Expr {
    /// The type this expression evaluates to.
    pub fn ty(&self) -> Type {
        match self {
            Expr::Column { ty, .. }
            | Expr::Literal { ty, .. }
            | Expr::Call { ty, .. }
            | Expr::Cast { ty, .. }
            | Expr::Subquery { ty, .. } => *ty,
        }
    }

    pub fn parse(v: &Value, schema_count: usize) -> Result<Expr, TranslateError> {
        let o = v.as_object().ok_or(TranslateError::ExprNotObject)?;

        if let Some(c) = o.get("column") {
            let index = c
                .as_u64()
                .ok_or_else(|| TranslateError::MalformedShape("column index not a number".into()))? as u32;
            let ty = Type::parse(o.get("type").and_then(Value::as_str).unwrap_or(""))?;
            return Ok(Expr::Column { index, ty });
        }

        let op = o.get("operator").and_then(Value::as_str).unwrap_or("").to_string();
        let ty_str = o.get("type").and_then(Value::as_str).unwrap_or("");
        let ty = Type::parse(ty_str)?;

        if o.contains_key("query") {
            return Self::parse_subquery(v, &op, ty, schema_count);
        }

        let operand: &[Value] = o.get("operand").and_then(Value::as_array).map(Vec::as_slice).unwrap_or(&[]);
        if operand.is_empty() {
            return Self::literal(op, ty_str, ty);
        }

        let args = operand.iter().map(|a| Expr::parse(a, schema_count)).collect::<Result<Vec<_>, _>>()?;

        if op == "CAST" {
            if args.len() != 1 {
                return Err(TranslateError::CastArity);
            }
            let operand = args.into_iter().next().expect("checked len == 1 above");
            return Ok(Expr::Cast { ty, operand: Box::new(operand) });
        }

        Ok(Expr::Call { operator: op, ty, operand: args })
    }

    fn literal(value: String, ty_str: &str, ty: Type) -> Result<Expr, TranslateError> {
        // `IrToRel.literal()` parses INTEGER/REAL values through `new BigDecimal(value)` and refuses
        // on `NumberFormatException`; BOOLEAN/VARCHAR/VARBINARY never fail to parse on their side.
        // `"NULL"` short-circuits before any type-specific parsing on both sides.
        if value != "NULL" && matches!(ty, Type::Integer | Type::Real) && value.parse::<f64>().is_err() {
            return Err(TranslateError::Literal(ty_str.to_string()));
        }
        // The frontend never emits a temporal literal (a literal cast arrives as a `q_conv_varchar_*`
        // call over a VARCHAR literal). Read as a string, one would make `'2020-01-01'` and
        // `'2020-1-1'` distinct constants although they are the same day, so a stray one is refused.
        if value != "NULL" && matches!(ty, Type::Date | Type::Time | Type::Timestamp | Type::Interval) {
            return Err(TranslateError::Literal(ty_str.to_string()));
        }
        Ok(Expr::Literal { value, ty })
    }

    fn parse_subquery(v: &Value, op: &str, ty: Type, schema_count: usize) -> Result<Expr, TranslateError> {
        let query = Relation::parse(field(v, "query")?, schema_count)?;
        match op {
            "EXISTS" | "$SCALAR_QUERY" => {
                Ok(Expr::Subquery { operator: op.to_string(), ty, operand: Vec::new(), query: Box::new(query) })
            }
            "IN" => {
                let operand = match v.get("operand").and_then(Value::as_array) {
                    None => Vec::new(),
                    Some(a) => a.iter().map(|e| Expr::parse(e, schema_count)).collect::<Result<_, _>>()?,
                };
                Ok(Expr::Subquery { operator: "IN".to_string(), ty, operand, query: Box::new(query) })
            }
            other => Err(TranslateError::Subquery(other.to_string())),
        }
    }
}

fn opt_expr(v: &Value, key: &str, schema_count: usize) -> Result<Option<Expr>, TranslateError> {
    match v.get(key) {
        None => Ok(None),
        Some(x) if x.is_null() => Ok(None),
        Some(x) => Ok(Some(Expr::parse(x, schema_count)?)),
    }
}

fn field<'v>(v: &'v Value, key: &str) -> Result<&'v Value, TranslateError> {
    v.get(key).ok_or_else(|| TranslateError::MalformedShape(format!("missing field `{key}`")))
}

fn array(v: &Value) -> Result<&Vec<Value>, TranslateError> {
    v.as_array().ok_or_else(|| TranslateError::MalformedShape("expected a JSON array".into()))
}

/// One table's schema: `{"name"?, "types", "key"?, "nullable"?, "guaranteed"?}`. `name` falls back
/// to `"t{index}"` -- the exact spelling `IrToRel.build()` uses for a nameless schema, and the
/// spelling `sqlsolver::ddl_from_ir` mints on the emitting side, so the two agree without a name
/// ever needing to travel on the wire. `key`/`nullable` default to empty (`IrToRel.java` never reads
/// either, but this port's integrity-constraint rewriting, `ic.rs`, does).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Schema {
    pub name: String,
    pub types: Vec<Type>,
    pub key: Vec<Vec<usize>>,
    pub nullable: Vec<bool>,
}

impl Schema {
    fn parse(v: &Value, index: usize) -> Result<Schema, TranslateError> {
        let name = v.get("name").and_then(Value::as_str).map(str::to_string).unwrap_or_else(|| format!("t{index}"));
        let types = array(field(v, "types")?)?
            .iter()
            .map(|t| {
                t.as_str()
                    .ok_or_else(|| TranslateError::MalformedShape("schema type entry not a string".into()))
                    .and_then(Type::parse)
            })
            .collect::<Result<_, _>>()?;
        let key = v
            .get("key")
            .and_then(Value::as_array)
            .map(|groups| {
                groups
                    .iter()
                    .filter_map(Value::as_array)
                    .map(|group| group.iter().filter_map(|i| i.as_u64().map(|n| n as usize)).collect())
                    .collect()
            })
            .unwrap_or_default();
        let nullable = v
            .get("nullable")
            .and_then(Value::as_array)
            .map(|a| a.iter().map(|b| b.as_bool().unwrap_or(false)).collect())
            .unwrap_or_default();
        Ok(Schema { name, types, key, nullable })
    }
}

/// The top-level `Input`: `{"schemas": [...], "queries": [Relation, Relation], "help": [String,
/// String]}`. `help` is dropped entirely (see the module doc) rather than modeled and ignored.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Input {
    pub schemas: Vec<Schema>,
    pub queries: [Relation; 2],
}

/// The deepest an `Input` may nest, counting every JSON object and array. Real plans stay far below
/// it -- the frontend writes `AND`/`OR` chains flat -- and the binary runs each row on a thread
/// whose stack takes every stage through a plan this deep.
pub const MAX_DEPTH: usize = 2_000;

/// The nesting depth of `v` (objects and arrays), walked without recursion so that measuring a
/// pathological value cannot overflow the stack it is meant to protect. Stops counting past
/// `cap`.
pub fn depth(v: &Value, cap: usize) -> usize {
    let mut deepest = 0;
    let mut stack: Vec<(&Value, usize)> = vec![(v, 1)];
    while let Some((v, d)) = stack.pop() {
        let children: Box<dyn Iterator<Item = &Value>> = match v {
            Value::Array(a) => Box::new(a.iter()),
            Value::Object(o) => Box::new(o.values()),
            _ => continue,
        };
        deepest = deepest.max(d);
        if deepest > cap {
            return deepest;
        }
        stack.extend(children.map(|c| (c, d + 1)));
    }
    deepest
}

impl Input {
    /// Refuses a plan nested deeper than [`MAX_DEPTH`] (`nesting-too-deep`) before reading it.
    pub fn parse(v: &Value) -> Result<Input, TranslateError> {
        if depth(v, MAX_DEPTH) > MAX_DEPTH {
            return Err(TranslateError::TooDeep);
        }
        let schemas: Vec<Schema> = v
            .get("schemas")
            .and_then(Value::as_array)
            .map(Vec::as_slice)
            .unwrap_or(&[])
            .iter()
            .enumerate()
            .map(|(i, s)| Schema::parse(s, i))
            .collect::<Result<_, _>>()?;
        let n = schemas.len();

        let queries = array(field(v, "queries")?)?;
        if queries.len() != 2 {
            return Err(TranslateError::MalformedShape(format!("expected 2 queries, found {}", queries.len())));
        }
        let q0 = Relation::parse(&queries[0], n)?;
        let q1 = Relation::parse(&queries[1], n)?;
        Ok(Input { schemas, queries: [q0, q1] })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// The same fixture `src/verify.rs`'s own tests use: one two-column table, a query that projects
    /// its second column, with the interesting numbering inside a subquery in the filter condition
    /// (whose own row starts at column 2). Reused here to prove this crate parses what that module
    /// already accepts as well-formed.
    fn sample_input(inner_key: Value) -> Value {
        let query = json!({ "project": {
            "target": [{ "column": 0, "type": "INTEGER" }],
            "source": { "filter": {
                "condition": {
                    "operator": "IN", "type": "BOOLEAN",
                    "operand": [{ "column": 1, "type": "VARCHAR" }],
                    "query": { "group": {
                        "keys": [inner_key], "function": [],
                        "source": { "project": {
                            "target": [{ "column": 3, "type": "VARCHAR" }],
                            "source": { "scan": 0 },
                        }},
                    }},
                },
                "source": { "scan": 0 },
            }},
        }});
        json!({
            "schemas": [{ "types": ["INTEGER", "VARCHAR"], "key": [], "nullable": [true, true], "guaranteed": [] }],
            // `Input.queries` is always a pair on the real wire (the two sides of an equivalence
            // check); reusing the same relation twice keeps this fixture faithful to that shape
            // rather than testing a one-query `Input` the real bridge never produces.
            "queries": [query.clone(), query],
            "help": ["", ""],
        })
    }

    #[test]
    fn parses_a_well_formed_query() {
        let input = sample_input(json!({ "column": 2, "type": "VARCHAR" }));
        let parsed = Input::parse(&input).expect("well-formed input must parse");
        assert_eq!(parsed.schemas.len(), 1);
        assert_eq!(parsed.schemas[0].types, vec![Type::Integer, Type::Varchar]);
        assert!(matches!(parsed.queries[0], Relation::Project { .. }));
        assert!(matches!(parsed.queries[1], Relation::Project { .. }));
    }

    #[test]
    fn refuses_a_non_object_relation() {
        // The bug `IrToRel.rel()` guards against with `r.isObject()`: a FROM-less "singleton" query
        // is a bare string on the wire, which this bridge has never been able to accept.
        let err = Relation::parse(&json!("singleton"), 1).unwrap_err();
        assert_eq!(err.to_string(), "rel-not-object");
    }

    #[test]
    fn refuses_scan_out_of_range() {
        let err = Relation::parse(&json!({ "scan": 5 }), 1).unwrap_err();
        assert_eq!(err.to_string(), "scan-out-of-range");
    }

    #[test]
    fn refuses_an_unknown_relation_tag() {
        // `IrToRel.rel()` has no `aggregate` case (only `group`) -- confirmed by grep, not assumed.
        let err = Relation::parse(&json!({ "aggregate": {} }), 1).unwrap_err();
        assert_eq!(err.to_string(), "rel:aggregate");
    }

    #[test]
    fn refuses_wrong_set_op_arity() {
        let err = Relation::parse(&json!({ "union": [{ "scan": 0 }] }), 1).unwrap_err();
        assert_eq!(err.to_string(), "union-arity");
    }

    #[test]
    fn refuses_malformed_collation() {
        let err = Relation::parse(
            &json!({ "sort": {
                "source": { "scan": 0 },
                "collation": [[0, "INTEGER"]],
            }}),
            1,
        )
        .unwrap_err();
        assert_eq!(err.to_string(), "collation-shape");
    }

    #[test]
    fn treats_a_zero_operand_call_as_a_literal() {
        let e = Expr::parse(&json!({ "operator": "NULL", "type": "INTEGER" }), 0).unwrap();
        assert!(matches!(e, Expr::Literal { ty: Type::Integer, .. }));
    }

    #[test]
    fn refuses_a_non_numeric_integer_literal() {
        let err = Expr::parse(&json!({ "operator": "abc", "type": "INTEGER" }), 0).unwrap_err();
        assert_eq!(err.to_string(), "literal:INTEGER");
    }

    #[test]
    fn reads_the_temporal_types_and_refuses_what_the_frontend_never_emits() {
        for t in ["DATE", "TIME", "TIMESTAMP", "INTERVAL"] {
            assert_eq!(Type::parse(t).unwrap().name(), t);
        }
        // TIMESTAMPTZ leaves the frontend as TIMESTAMP, so the name itself is unknown here.
        assert_eq!(Type::parse("TIMESTAMPTZ").unwrap_err().to_string(), "unknown-type:TIMESTAMPTZ");
        // A temporal literal is never emitted; read as a string it would be spelling-sensitive.
        let err = Expr::parse(&json!({ "operator": "2020-01-01", "type": "DATE" }), 0).unwrap_err();
        assert_eq!(err.to_string(), "literal:DATE");
        let null = Expr::parse(&json!({ "operator": "NULL", "type": "TIMESTAMP" }), 0).unwrap();
        assert!(matches!(null, Expr::Literal { ty: Type::Timestamp, .. }));
    }

    #[test]
    fn refuses_cast_with_wrong_arity() {
        let err = Expr::parse(
            &json!({
                "operator": "CAST", "type": "VARCHAR",
                "operand": [
                    {"operator": "1", "type": "INTEGER"},
                    {"operator": "2", "type": "INTEGER"},
                ],
            }),
            0,
        )
        .unwrap_err();
        assert_eq!(err.to_string(), "cast-arity");
    }

    #[test]
    fn refuses_an_unsupported_subquery_operator() {
        let err = Expr::parse(&json!({ "operator": "ANY", "type": "BOOLEAN", "query": { "scan": 0 } }), 1)
            .unwrap_err();
        assert_eq!(err.to_string(), "subquery:ANY");
    }

    #[test]
    fn refuses_a_non_literal_values_cell() {
        let err = Relation::parse(
            &json!({ "values": {
                "schema": ["INTEGER"],
                "content": [[{ "operator": "ABS", "type": "INTEGER", "operand": [{ "operator": "1", "type": "INTEGER" }] }]],
            }}),
            0,
        )
        .unwrap_err();
        assert_eq!(err.to_string(), "values-nonliteral");
    }
}
