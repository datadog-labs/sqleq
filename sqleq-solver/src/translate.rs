// Unless explicitly stated otherwise all files in this repository are licensed under the
// Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/).
// Copyright 2026-Present Datadog, Inc.

//! `Relation`/`Expr` (from `ir.rs`) to `UTerm` translation. Follows the Java reference algorithm
//! (`UExprConcreteTranslator.java`), but departs from it in deliberate ways documented inline as
//! they come up -- among them: a single `Value{is_null,value}` pair replaces Java's general `ComposedUTerm`, and
//! set-op translation mints a fresh output var and re-projects both branches onto it (Java's
//! substitution-by-position approach only works when both branches expose the same number of
//! existential vars, which isn't guaranteed -- e.g. a `Join`-shaped branch unioned with a
//! `Project`-shaped one).
//!
//! ## NULL representation
//!
//! Every value-producing translation returns a [`Value`]: a term that might be null (`is_null`,
//! 0/1-valued) paired with a term that's only meaningful when it isn't (`value`). `value` must never
//! be read directly -- always through `value_eq`, the one place a `Value` is bound to a target
//! column/position. This is what makes `UConst::Null`-as-a-placeholder safe: a null literal's `value`
//! is a dummy (`Const(Int(0))`), and that's fine because nothing ever looks at it without checking
//! `is_null` first.
//!
//! A bare column reference is the only place nullness is *derived*, via equality with the `Null`
//! sentinel (see `column_value`); everywhere else, `is_null` is tracked compositionally by each
//! operator's own formula, never re-derived after the fact.
//!
//! ## Three-valued logic
//!
//! A boolean expression translates to a [`Truth`]: two mutually exclusive 0/1 terms, `t` (the
//! expression is TRUE) and `u` (it is UNKNOWN); FALSE is neither. Every connective follows SQL's
//! truth tables on the pair, so `NOT` of an UNKNOWN stays UNKNOWN. A `WHERE`/`ON`/`CASE WHEN`
//! position reads `t` alone (UNKNOWN rejects the row), and a boolean in value position becomes a
//! [`Value`] with `is_null = u`. Collapsing UNKNOWN to FALSE *before* a `NOT` -- what a single 0/1
//! term per predicate forces -- would make `NOT (a = 1)` true on a NULL `a`, a false-proof channel
//! (`NOT (a = 1)` would equal `a IS DISTINCT FROM 1`), and would get `NOT IN` wrong whenever the
//! subquery yields a NULL.
//!
//! ## Types
//!
//! A `UTerm` carries no types, so the translation keeps every distinction a type makes in the
//! terms themselves. A constant keeps its own (see [`UConst`]: `1`, `1.0` and `1.00` are three
//! values). Every uninterpreted symbol is named after the IR types of its operands and of its
//! result (see `typed_symbol`), because Postgres picks a function by its name and argument types:
//! `/` on two integers truncates and on a decimal does not, and `CAST(1 AS TEXT)` and
//! `CAST(1.0 AS TEXT)` spell different strings. And SQL's `=` is read as identity of the two
//! values -- the reading that lets normalization substitute one side for the other -- only between
//! two values of one type on which `=` *is* identity; anywhere else (two decimals, an integer
//! against a decimal, two intervals) it compares the values' images under a key function, so that
//! `a = 2.0` says nothing about what `a` *is* (see `sql_eq`).

use crate::ir::{AggCall, Expr, JoinKind, Relation, Schema, TranslateError, Type};
use crate::uterm::{mk_add, mk_mul, mk_neg, mk_or, mk_squash, mk_sum, PredKind, UConst, UTerm, UVar};

/// The key function SQL's `=` compares two numbers (INTEGER or REAL, in any mix) through: their
/// exact numeric value, whatever their type or scale. Normalization and the evaluator interpret it
/// as exactly that, and nothing else does; see `sql_eq`.
pub const NUMERIC_EQ_KEY: &str = "eq:numeric";

/// The name of an uninterpreted symbol for `base` over operands of the IR types `args`, with
/// result type `ret`. Postgres resolves a function or operator by its name *and* its argument
/// types, so a symbol that left the types out would stand for several functions at once, and an
/// uninterpreted symbol is sound only while it stands for one: the real function must be one of its
/// interpretations.
fn typed_symbol(base: &str, args: &[Type], ret: Type) -> String {
    let args: Vec<&str> = args.iter().map(|t| t.name()).collect();
    format!("{base}({})->{}", args.join(","), ret.name())
}

fn types_of(operand: &[Expr]) -> Vec<Type> {
    operand.iter().map(Expr::ty).collect()
}

/// The key function through which SQL's `=` between a value of type `a` and one of type `b` is
/// read, or `None` when it is identity of the two values. It is identity only between two values of
/// one type whose `=` holds exactly when the values are the same: integers, strings (under a
/// deterministic collation, which compares bytes), booleans, dates, times and timestamps, and the
/// opaque VARBINARY. It is not between two decimals (`2.0 = 2.00`, yet they print, cast and divide
/// differently), an integer and a decimal, or two intervals (`'1 day' = '24 hours'`). There the two
/// sides are compared through a key, which a substitution cannot see through, so an equality never
/// licenses putting one value where the other was.
fn eq_key(a: Type, b: Type) -> Option<String> {
    let numeric = |t: Type| matches!(t, Type::Integer | Type::Real);
    if a == b && !matches!(a, Type::Real | Type::Interval) {
        return None;
    }
    if numeric(a) && numeric(b) {
        return Some(NUMERIC_EQ_KEY.to_string());
    }
    let (x, y) = if a.name() <= b.name() { (a, b) } else { (b, a) };
    Some(if x == y { format!("eq:{}", x.name()) } else { format!("eq:{}|{}", x.name(), y.name()) })
}

/// SQL's `a = b` on two non-NULL values of the IR types `ta` and `tb`, as a 0/1 term: identity of
/// the values where that is what `=` means, their images under [`eq_key`]'s key otherwise. `None`
/// for a type the IR could not resolve, which is compared through a key of its own.
fn sql_eq(a: UTerm, ta: Option<Type>, b: UTerm, tb: Option<Type>) -> UTerm {
    sql_equality(PredKind::Eq, a, ta, b, tb)
}

/// [`sql_eq`], or with `kind` `Ne` its negation, SQL's `<>`.
fn sql_equality(kind: PredKind, a: UTerm, ta: Option<Type>, b: UTerm, tb: Option<Type>) -> UTerm {
    let key = match (ta, tb) {
        (Some(ta), Some(tb)) => eq_key(ta, tb),
        _ => Some("eq:unresolved".to_string()),
    };
    let args = match key {
        None => vec![a, b],
        Some(k) => vec![UTerm::Func { name: k.clone(), args: vec![a] }, UTerm::Func { name: k, args: vec![b] }],
    };
    UTerm::Pred { kind, args }
}

/// A value that might be null. See the module doc for the invariant governing `value`.
#[derive(Debug, Clone)]
pub struct Value {
    pub is_null: UTerm,
    pub value: UTerm,
}

impl Value {
    fn not_null(value: UTerm) -> Value {
        Value { is_null: UTerm::Const(UConst::Int(0)), value }
    }

    /// A placeholder value for NULL literals. Safe only because nothing ever reads `.value` without
    /// checking `.is_null` first (see the module doc and [`value_eq`]).
    fn null() -> Value {
        Value { is_null: UTerm::Const(UConst::Int(1)), value: UTerm::Const(UConst::Int(0)) }
    }
}

/// `target == Null` when `v` is null, `target == v.value` otherwise, as one identity equality
/// `[target = v′]` with `v′` the value's NULL-sentinel form ([`sentinel_value`]). This is the *only*
/// sanctioned way to bind a `Value` to an output column -- a bare `[target = v.value]` would wrongly
/// force `target` to a real value (e.g. `0`) instead of `Null` whenever `v.is_null` might hold.
///
/// Keeping it one predicate matters: the null split lives in value position, which normalization
/// leaves alone, instead of as a two-branch sum at the multiplicity level, where a projection of
/// n such columns distributes into 2^n products.
fn value_eq(target: &UTerm, v: &Value) -> UTerm {
    UTerm::Pred { kind: PredKind::Eq, args: vec![target.clone(), sentinel_value(v)] }
}

/// The value with NULL represented by the `Null` constant: `v.value` itself when its nullness is read
/// off it (a bare column already holds `Null`) or when it is never null; otherwise a selection
/// `null·Null + ¬null·value` between exclusive guards (the CASE idiom `eval` evaluates, and Java's
/// `ComposedUTerm` flattening in value position).
fn sentinel_value(v: &Value) -> UTerm {
    match &v.is_null {
        UTerm::Const(UConst::Int(0)) => v.value.clone(),
        UTerm::Const(UConst::Int(1)) => UTerm::Const(UConst::Null),
        null if *null == is_null_of(&v.value) => v.value.clone(),
        null => mk_add([
            mk_mul([null.clone(), UTerm::Const(UConst::Null)]),
            mk_mul([mk_neg(null.clone()), v.value.clone()]),
        ]),
    }
}

/// A three-valued boolean: `t` is 1 iff TRUE, `f` is 1 iff FALSE, never both; UNKNOWN is neither.
/// Carrying FALSE rather than UNKNOWN keeps every connective sum-free inside products -- `NOT`
/// swaps the two, `AND` is `Π t` / `‖Σ f‖`, `OR` the dual -- which is what keeps sum-of-products
/// normalization from distributing a product of `(t + u)` sums into 2^n terms. See the module doc.
#[derive(Debug, Clone)]
pub struct Truth {
    pub t: UTerm,
    pub f: UTerm,
}

impl Truth {
    /// A predicate that is never UNKNOWN (`IS NULL`, `EXISTS`, `IS DISTINCT FROM`, ...).
    fn two_valued(t: UTerm) -> Truth {
        Truth { f: mk_neg(t.clone()), t }
    }

    /// A comparison-like predicate: UNKNOWN when `unknown` holds, otherwise whether `holds` (0/1).
    fn guarded(unknown: UTerm, holds: UTerm) -> Truth {
        let known = mk_neg(unknown);
        Truth { t: mk_mul([known.clone(), holds.clone()]), f: mk_mul([known, mk_neg(holds)]) }
    }

    /// UNKNOWN: neither TRUE nor FALSE.
    pub fn u(&self) -> UTerm {
        mk_mul([mk_neg(self.t.clone()), mk_neg(self.f.clone())])
    }
}

/// A value used as a boolean, as a 0/1 term. Values that are structurally 0/1 already pass through;
/// anything else (an uninterpreted function's result) is compared against 1, so a predicate can never
/// contribute a multiplicity other than 0 or 1.
fn truthy(value: UTerm) -> UTerm {
    match value {
        UTerm::Pred { .. } | UTerm::Squash(_) | UTerm::Neg(_) | UTerm::Const(UConst::Int(0 | 1)) => value,
        other => UTerm::Pred { kind: PredKind::Eq, args: vec![other, UTerm::Const(UConst::Int(1))] },
    }
}

fn is_null_of(t: &UTerm) -> UTerm {
    UTerm::Pred { kind: PredKind::Eq, args: vec![t.clone(), UTerm::Const(UConst::Null)] }
}

fn is_null_row(vars: &[UVar]) -> UTerm {
    mk_mul(vars.iter().map(|v| is_null_of(&UTerm::Var(v.clone()))))
}

/// The only place nullness is *derived* rather than tracked compositionally: a bare column is null
/// iff it equals the `Null` sentinel.
fn column_value(v: UVar) -> Value {
    let t = UTerm::Var(v);
    Value { is_null: is_null_of(&t), value: t }
}

/// Functions that are NULL exactly when some argument is: strict (NULL in, NULL out) *and* never NULL
/// on non-NULL input. Only these may derive their nullness from their arguments; everything else goes
/// through [`Translator::opaque_value`]. `QCASTk` are the frontend's uninterpreted casts
/// (`src/casts.rs`), and a cast of a non-NULL value is never NULL.
fn is_null_exactly_on_null_input(op: &str) -> bool {
    matches!(op, "UPPER" | "LOWER" | "ABS" | "SUBSTRING" | "DATE" | "DATE_TRUNC" | "TO_CHAR" | "TO_TIMESTAMP" | "CARDINALITY" | "NLEVEL")
        || op.strip_prefix("QCAST").is_some_and(|k| !k.is_empty() && k.bytes().all(|b| b.is_ascii_digit()))
        // The frontend's temporal conversions and temporal operators: Postgres casts and the
        // date/time operators are all strict.
        || op.starts_with("q_conv_")
        || op.starts_with("q_arith_")
}

/// The operators with a three-valued meaning of their own (see [`Translator::truth_call`]), at the
/// arities that meaning is defined for.
fn is_boolean_operator(op: &str, arity: usize) -> bool {
    match op {
        "AND" | "OR" => true,
        "NOT" | "IS NULL" | "IS NOT NULL" | "IS NOT TRUE" => arity == 1,
        "=" | "<>" | "<" | "<=" | ">" | ">=" | "IS DISTINCT FROM" | "LIKE" | "= ANY" => arity == 2,
        _ => false,
    }
}

fn pred_kind(op: &str) -> PredKind {
    match op {
        "=" => PredKind::Eq,
        "<>" => PredKind::Ne,
        "<" => PredKind::Lt,
        "<=" => PredKind::Le,
        ">" => PredKind::Gt,
        ">=" => PredKind::Ge,
        // Every call site matches one of these six strings in its own guard before calling this;
        // reaching here would be this module's own bug, not a possible shape of real input.
        other => unreachable!("pred_kind called with non-comparison operator {other:?}"),
    }
}

/// Column-resolution scope: `local[i]` is the `UVar` standing for logical column `base + i`.
/// `base` is constant across a non-subquery-crossing relation subtree and only advances when
/// translation descends into a nested subquery.
struct Scope {
    base: usize,
    local: Vec<UVar>,
}

impl Scope {
    fn resolve(&self, index: u32) -> Result<UVar, TranslateError> {
        let index = index as usize;
        if index < self.base {
            return Err(TranslateError::Correlated);
        }
        self.local.get(index - self.base).cloned().ok_or(TranslateError::ColumnOutOfRange)
    }
}

/// The result of translating one `Relation`: its indicator term, the vars its output columns are
/// bound to (`local`), and the existential vars a parent must sum over to consume it (`exposed`).
#[derive(Debug)]
struct Translated {
    term: UTerm,
    local: Vec<UVar>,
    exposed: Vec<UVar>,
}

pub struct Translator<'s> {
    schemas: &'s [Schema],
    /// `widths[i]` is the column count of `Base(i)`; its length is the next fresh id.
    widths: Vec<usize>,
}

impl<'s> Translator<'s> {
    pub fn new(schemas: &'s [Schema]) -> Self {
        Translator { schemas, widths: Vec::new() }
    }

    fn fresh_var(&mut self, width: usize) -> UVar {
        let v = UVar::Base(self.widths.len() as u32);
        self.widths.push(width);
        v
    }

    fn rel(&mut self, r: &Relation, base: usize) -> Result<Translated, TranslateError> {
        match r {
            Relation::Scan(i) => self.scan(*i),
            Relation::Values { schema, content } => self.values(schema, content),
            Relation::Filter { source, condition } => self.filter(source, condition, base),
            Relation::Project { source, target } => self.project(source, target, base),
            Relation::Distinct(source) => self.distinct(source, base),
            Relation::Join { left, right, kind, condition } => self.join(left, right, *kind, condition, base),
            Relation::Group { source, keys, function } => self.group(source, keys, function, base),
            Relation::Sort { source, offset, limit, .. } => {
                if offset.is_some() || limit.is_some() {
                    return Err(TranslateError::UnsupportedSort);
                }
                self.rel(source, base)
            }
            Relation::Union(sides) => self.set_op(SetOpKind::Union, sides, base),
            Relation::Except(sides) => self.set_op(SetOpKind::Except, sides, base),
            Relation::Intersect(sides) => self.set_op(SetOpKind::Intersect, sides, base),
        }
    }

    fn scan(&mut self, i: usize) -> Result<Translated, TranslateError> {
        let schema = self.schemas.get(i).ok_or(TranslateError::ScanOutOfRange)?;
        let var = self.fresh_var(schema.types.len());
        let local: Vec<UVar> = (0..schema.types.len() as u32).map(|idx| UVar::proj(idx, var.clone())).collect();
        let term = UTerm::Table { name: schema.name.clone(), var: var.clone() };
        Ok(Translated { term, local, exposed: vec![var] })
    }

    fn values(&mut self, schema: &[Type], content: &[Vec<Expr>]) -> Result<Translated, TranslateError> {
        let var = self.fresh_var(schema.len());
        let local: Vec<UVar> = (0..schema.len() as u32).map(|idx| UVar::proj(idx, var.clone())).collect();
        let mut rows = Vec::with_capacity(content.len());
        for row in content {
            if row.len() != local.len() {
                return Err(TranslateError::MalformedShape("values row width does not match schema".into()));
            }
            let mut factors = Vec::with_capacity(row.len());
            for (col, cell) in local.iter().zip(row.iter()) {
                let v = match cell {
                    Expr::Literal { value, ty } => self.literal_value(value, *ty)?,
                    // `ir::Relation::parse` already guarantees every `values` cell is a literal
                    // before this stage ever runs.
                    _ => return Err(TranslateError::ValuesNonLiteral),
                };
                factors.push(value_eq(&UTerm::Var(col.clone()), &v));
            }
            rows.push(mk_mul(factors));
        }
        Ok(Translated { term: mk_add(rows), local, exposed: vec![var] })
    }

    fn filter(&mut self, source: &Relation, condition: &Expr, base: usize) -> Result<Translated, TranslateError> {
        let src = self.rel(source, base)?;
        let scope = Scope { base, local: src.local.clone() };
        let cond = self.translate_predicate(condition, &scope)?;
        Ok(Translated { term: mk_mul([src.term, cond]), local: src.local, exposed: src.exposed })
    }

    /// Shared tail for `Project` and `Distinct`: mint a fresh output var, existentially close over
    /// the source's exposed vars, bind each output column via [`value_eq`].
    fn project_values(&mut self, src: Translated, values: Vec<Value>) -> Translated {
        let out = self.fresh_var(values.len());
        let local: Vec<UVar> = (0..values.len() as u32).map(|i| UVar::proj(i, out.clone())).collect();
        let mut factors = vec![src.term];
        for (col, v) in local.iter().zip(values.iter()) {
            factors.push(value_eq(&UTerm::Var(col.clone()), v));
        }
        let term = mk_sum(src.exposed, mk_mul(factors));
        Translated { term, local, exposed: vec![out] }
    }

    fn project(&mut self, source: &Relation, target: &[Expr], base: usize) -> Result<Translated, TranslateError> {
        let src = self.rel(source, base)?;
        let scope = Scope { base, local: src.local.clone() };
        let values: Vec<Value> = target.iter().map(|e| self.translate_value(e, &scope)).collect::<Result<_, _>>()?;
        Ok(self.project_values(src, values))
    }

    /// `Distinct` = `Squash` of an identity projection: reuses `project_values`'s shape with each
    /// source column passed through unchanged, then clamps the result to 0/1.
    fn distinct(&mut self, source: &Relation, base: usize) -> Result<Translated, TranslateError> {
        let src = self.rel(source, base)?;
        let values: Vec<Value> = src.local.iter().cloned().map(column_value).collect();
        let projected = self.project_values(src, values);
        Ok(Translated { term: mk_squash(projected.term), local: projected.local, exposed: projected.exposed })
    }

    fn join(
        &mut self,
        left: &Relation,
        right: &Relation,
        kind: JoinKind,
        condition: &Expr,
        base: usize,
    ) -> Result<Translated, TranslateError> {
        let l = self.rel(left, base)?;
        let r = self.rel(right, base)?;
        let local: Vec<UVar> = l.local.iter().chain(r.local.iter()).cloned().collect();
        let scope = Scope { base, local: local.clone() };
        let cond = self.translate_predicate(condition, &scope)?;

        let matched = mk_mul([l.term.clone(), r.term.clone(), cond.clone()]);
        let mut branches = vec![matched];

        // LEFT/FULL: rows of `l` with no matching `r` row, padded with an all-null `r` side.
        if matches!(kind, JoinKind::Left | JoinKind::Full) {
            let exists_match = mk_squash(mk_sum(r.exposed.clone(), mk_mul([r.term.clone(), cond.clone()])));
            branches.push(mk_mul([l.term.clone(), mk_neg(exists_match), is_null_row(&r.local)]));
        }
        // RIGHT/FULL: the symmetric case, rows of `r` with no matching `l` row.
        if matches!(kind, JoinKind::Right | JoinKind::Full) {
            let exists_match = mk_squash(mk_sum(l.exposed.clone(), mk_mul([l.term.clone(), cond.clone()])));
            branches.push(mk_mul([r.term.clone(), mk_neg(exists_match), is_null_row(&l.local)]));
        }

        let term = mk_add(branches);
        let exposed: Vec<UVar> = l.exposed.into_iter().chain(r.exposed).collect();
        Ok(Translated { term, local, exposed })
    }

    fn group(
        &mut self,
        source: &Relation,
        keys: &[Expr],
        function: &[AggCall],
        base: usize,
    ) -> Result<Translated, TranslateError> {
        let src = self.rel(source, base)?;
        let scope = Scope { base, local: src.local.clone() };

        let mut key_vars = Vec::with_capacity(keys.len());
        for k in keys {
            match k {
                Expr::Column { index, .. } => key_vars.push(scope.resolve(*index)?),
                _ => return Err(TranslateError::GroupKeyNonRef),
            }
        }

        let out = self.fresh_var(key_vars.len() + function.len());
        let key_out: Vec<UVar> = (0..key_vars.len() as u32).map(|i| UVar::proj(i, out.clone())).collect();

        let key_eq = mk_mul(key_vars.iter().zip(key_out.iter()).map(|(kv, ov)| UTerm::Pred {
            kind: PredKind::Eq,
            args: vec![UTerm::Var(kv.clone()), UTerm::Var(ov.clone())],
        }));
        let group_by_term = mk_mul([src.term.clone(), key_eq]);

        // Scalar aggregation (no GROUP BY keys) must produce exactly one group even over an empty
        // input -- forcing presence unconditionally true is what makes that hold, rather than the
        // group vanishing the way a non-empty-keys existential check would (the historical bug class
        // `qed-scalar-agg-bug-fix` fixed on the Rust QED prover's own Group handling).
        let key_presence = if keys.is_empty() { UTerm::Const(UConst::Int(1)) } else { mk_squash(mk_sum(src.exposed.clone(), group_by_term.clone())) };

        let mut conjuncts = vec![key_presence];
        let mut local = key_out;
        for call in function {
            let col = UVar::proj(local.len() as u32, out.clone());
            conjuncts.push(self.agg_eq(call, &group_by_term, &src.exposed, &scope, &col)?);
            local.push(col);
        }

        Ok(Translated { term: mk_mul(conjuncts), local, exposed: vec![out] })
    }

    /// An aggregate's lone operand, required to be a bare column reference (matching the Java
    /// translator, which only ever indexes a `RexInputRef` here -- `SUM(a+b)` etc. is out of scope).
    fn agg_arg(operand: &[Expr], scope: &Scope) -> Result<Value, TranslateError> {
        match operand.first() {
            Some(Expr::Column { index, .. }) => Ok(column_value(scope.resolve(*index)?)),
            _ => Err(TranslateError::AggArgNonRef),
        }
    }

    fn agg_eq(
        &mut self,
        call: &AggCall,
        group_by_term: &UTerm,
        source_exposed: &[UVar],
        scope: &Scope,
        col: &UVar,
    ) -> Result<UTerm, TranslateError> {
        if call.distinct {
            return Err(TranslateError::DistinctAggregateUnsupported);
        }
        let outcol = UTerm::Var(col.clone());
        match call.operator.as_str() {
            "COUNT" => {
                let count = if call.operand.is_empty() {
                    mk_sum(source_exposed.to_vec(), group_by_term.clone())
                } else {
                    let x = Self::agg_arg(&call.operand, scope)?;
                    mk_sum(source_exposed.to_vec(), mk_mul([group_by_term.clone(), mk_neg(x.is_null)]))
                };
                Ok(value_eq(&outcol, &Value::not_null(count)))
            }
            "SUM" | "AVG" => {
                let x = Self::agg_arg(&call.operand, scope)?;
                let arg_ty: Vec<Type> = types_of(&call.operand);
                let notnull_x = mk_neg(x.is_null.clone());
                let nx = mk_sum(source_exposed.to_vec(), mk_mul([group_by_term.clone(), notnull_x.clone()]));
                let sum_x = mk_sum(source_exposed.to_vec(), mk_mul([group_by_term.clone(), notnull_x, x.value]));
                let is_null = UTerm::Pred { kind: PredKind::Eq, args: vec![nx.clone(), UTerm::Const(UConst::Int(0))] };
                let value = if call.operator == "SUM" {
                    sum_x
                } else {
                    // Not `/` of the two: `avg` of integers is a decimal, while `/` of the
                    // integer sum by the count truncates. Its own symbol, typed like any other.
                    UTerm::Func { name: typed_symbol("avg", &arg_ty, call.ty), args: vec![sum_x, nx] }
                };
                Ok(value_eq(&outcol, &Value { is_null, value }))
            }
            "MAX" | "MIN" => {
                let x = Self::agg_arg(&call.operand, scope)?;
                let notnull_x = mk_neg(x.is_null.clone());
                let some_nonnull =
                    mk_squash(mk_sum(source_exposed.to_vec(), mk_mul([group_by_term.clone(), notnull_x.clone()])));
                let beyond = if call.operator == "MAX" { PredKind::Gt } else { PredKind::Lt };
                let some_beyond = mk_squash(mk_sum(
                    source_exposed.to_vec(),
                    mk_mul([group_by_term.clone(), notnull_x.clone(), UTerm::Pred { kind: beyond, args: vec![x.value.clone(), outcol.clone()] }]),
                ));
                let some_equal = mk_squash(mk_sum(
                    source_exposed.to_vec(),
                    mk_mul([group_by_term.clone(), notnull_x, UTerm::Pred { kind: PredKind::Eq, args: vec![x.value, outcol.clone()] }]),
                ));
                let extremum_holds = mk_mul([mk_neg(some_beyond), some_equal]);
                Ok(mk_add([
                    mk_mul([some_nonnull.clone(), extremum_holds]),
                    mk_mul([mk_neg(some_nonnull), is_null_of(&outcol)]),
                ]))
            }
            other => Err(TranslateError::Aggregate(other.to_string())),
        }
    }

    fn set_op(&mut self, kind: SetOpKind, sides: &[Box<Relation>; 2], base: usize) -> Result<Translated, TranslateError> {
        let l = self.rel(&sides[0], base)?;
        let r = self.rel(&sides[1], base)?;
        if l.local.len() != r.local.len() {
            return Err(TranslateError::MalformedShape("set-op branches have different column counts".into()));
        }
        let out = self.fresh_var(l.local.len());
        let out_local: Vec<UVar> = (0..l.local.len() as u32).map(|i| UVar::proj(i, out.clone())).collect();
        let l_term = set_side_term(l.term, l.exposed, &l.local, &out_local);
        let r_term = set_side_term(r.term, r.exposed, &r.local, &out_local);
        let term = match kind {
            SetOpKind::Union => mk_add([l_term, r_term]),
            SetOpKind::Except => mk_squash(mk_mul([l_term, mk_neg(r_term)])),
            SetOpKind::Intersect => mk_squash(mk_mul([l_term, r_term])),
        };
        Ok(Translated { term, local: out_local, exposed: vec![out] })
    }

    // -- predicate/value context dispatch --------------------------------------------------------

    /// A boolean expression in a filtering position (`WHERE`, `ON`, `CASE WHEN`): only TRUE passes.
    fn translate_predicate(&mut self, e: &Expr, scope: &Scope) -> Result<UTerm, TranslateError> {
        Ok(self.translate_truth(e, scope)?.t)
    }

    fn translate_truth(&mut self, e: &Expr, scope: &Scope) -> Result<Truth, TranslateError> {
        match e {
            Expr::Call { operator, operand, .. } if is_boolean_operator(operator, operand.len()) => {
                self.truth_call(operator, operand, scope)
            }
            Expr::Subquery { operator, query, .. } if operator == "EXISTS" => {
                Ok(Truth::two_valued(self.exists_predicate(query, scope)?))
            }
            Expr::Subquery { operator, operand, query, .. } if operator == "IN" => self.in_truth(operand, query, scope),
            _ => {
                let v = self.translate_value(e, scope)?;
                Ok(Truth::guarded(v.is_null, truthy(v.value)))
            }
        }
    }

    /// The operators [`Self::truth_call`] gives a three-valued meaning of their own; every other call is a
    /// value first and read as a boolean afterwards.
    fn truth_call(&mut self, operator: &str, operand: &[Expr], scope: &Scope) -> Result<Truth, TranslateError> {
        match operator {
            // TRUE iff all are TRUE; FALSE iff any is FALSE.
            "AND" => {
                let parts: Vec<Truth> = operand.iter().map(|o| self.translate_truth(o, scope)).collect::<Result<_, _>>()?;
                let t = mk_mul(parts.iter().map(|p| p.t.clone()));
                let f = mk_squash(mk_add(parts.iter().map(|p| p.f.clone())));
                Ok(Truth { t, f })
            }
            // TRUE iff any is TRUE; FALSE iff all are FALSE.
            "OR" => {
                let parts: Vec<Truth> = operand.iter().map(|o| self.translate_truth(o, scope)).collect::<Result<_, _>>()?;
                let t = mk_squash(mk_add(parts.iter().map(|p| p.t.clone())));
                let f = mk_mul(parts.iter().map(|p| p.f.clone()));
                Ok(Truth { t, f })
            }
            // TRUE and FALSE swap; UNKNOWN stays UNKNOWN.
            "NOT" => {
                let p = self.translate_truth(&operand[0], scope)?;
                Ok(Truth { t: p.f, f: p.t })
            }
            "=" | "<>" | "<" | "<=" | ">" | ">=" => {
                let a = self.translate_value(&operand[0], scope)?;
                let b = self.translate_value(&operand[1], scope)?;
                let (ta, tb) = (Some(operand[0].ty()), Some(operand[1].ty()));
                let holds = match pred_kind(operator) {
                    kind @ (PredKind::Eq | PredKind::Ne) => sql_equality(kind, a.value, ta, b.value, tb),
                    // An order comparison is never read as identity, so it needs no key: values
                    // equal under `=` are simply neither less than the other.
                    kind => UTerm::Pred { kind, args: vec![a.value, b.value] },
                };
                Ok(Truth::guarded(mk_or(a.is_null, b.is_null), holds))
            }
            "IS NULL" => Ok(Truth::two_valued(self.translate_value(&operand[0], scope)?.is_null)),
            "IS NOT NULL" => Ok(Truth::two_valued(mk_neg(self.translate_value(&operand[0], scope)?.is_null))),
            "IS DISTINCT FROM" => {
                let a = self.translate_value(&operand[0], scope)?;
                let b = self.translate_value(&operand[1], scope)?;
                let both_null = mk_mul([a.is_null.clone(), b.is_null.clone()]);
                let both_nonnull = mk_mul([mk_neg(a.is_null), mk_neg(b.is_null)]);
                let eq = sql_eq(a.value, Some(operand[0].ty()), b.value, Some(operand[1].ty()));
                Ok(Truth::two_valued(mk_neg(mk_add([both_null, mk_mul([both_nonnull, eq])]))))
            }
            "IS NOT TRUE" => Ok(Truth::two_valued(mk_neg(self.translate_truth(&operand[0], scope)?.t))),
            "LIKE" => {
                let a = self.translate_value(&operand[0], scope)?;
                let b = self.translate_value(&operand[1], scope)?;
                let name = typed_symbol("like", &types_of(operand), Type::Boolean);
                let holds = truthy(UTerm::Func { name, args: vec![a.value, b.value] });
                Ok(Truth::guarded(mk_or(a.is_null, b.is_null), holds))
            }
            // `x = ANY(arr)` is TRUE only with both operands non-NULL (`NULL = ANY(..)` and
            // `x = ANY(NULL)` are never TRUE), so TRUE carries that guard. Whether a non-TRUE
            // outcome is UNKNOWN or FALSE (an empty array, a NULL element) is left to an
            // uninterpreted symbol over the operands' (null flag, value) pairs: the real split is one
            // of its interpretations.
            "= ANY" => {
                let a = self.translate_value(&operand[0], scope)?;
                let b = self.translate_value(&operand[1], scope)?;
                let args = vec![a.is_null.clone(), a.value, b.is_null.clone(), b.value];
                let name = typed_symbol("= ANY", &types_of(operand), Type::Boolean);
                let holds = truthy(UTerm::Func { name: name.clone(), args: args.clone() });
                let t = mk_mul([mk_neg(a.is_null), mk_neg(b.is_null), holds]);
                let unknown = truthy(UTerm::Func { name: format!("unknown:{name}"), args });
                Ok(Truth { f: mk_mul([mk_neg(t.clone()), mk_neg(unknown)]), t })
            }
            other => unreachable!("truth_call reached with non-boolean operator {other:?}"),
        }
    }

    fn exists_predicate(&mut self, query: &Relation, scope: &Scope) -> Result<UTerm, TranslateError> {
        let inner = self.rel(query, scope.base + scope.local.len())?;
        Ok(mk_squash(mk_sum(inner.exposed, inner.term)))
    }

    /// `(l1, ..) IN (subquery)`. TRUE iff some row equals the operand column-by-column. UNKNOWN iff
    /// no row does but some row *might*: every column either equal or with a NULL on one side, and at
    /// least one NULL. This is the case that makes `x NOT IN (..)` reject every row once the subquery
    /// yields a NULL, and why `NOT IN` is not `NOT EXISTS`.
    fn in_truth(&mut self, operand: &[Expr], query: &Relation, scope: &Scope) -> Result<Truth, TranslateError> {
        let lhs: Vec<Value> = operand.iter().map(|e| self.translate_value(e, scope)).collect::<Result<_, _>>()?;
        let inner = self.rel(query, scope.base + scope.local.len())?;
        if inner.local.len() != lhs.len() {
            return Err(TranslateError::MalformedShape("IN operand arity does not match subquery width".into()));
        }
        let inner_types = query.output_types(self.schemas);
        let mut eqs = Vec::with_capacity(lhs.len());
        let mut nulls = Vec::with_capacity(lhs.len());
        for (i, (l, col)) in lhs.iter().zip(inner.local.iter()).enumerate() {
            let rhs = column_value(col.clone());
            let null = mk_or(l.is_null.clone(), rhs.is_null);
            let eq = sql_eq(l.value.clone(), Some(operand[i].ty()), rhs.value, inner_types.get(i).copied().flatten());
            eqs.push(mk_mul([mk_neg(null.clone()), eq]));
            nulls.push(null);
        }
        let matches = mk_mul(eqs.iter().cloned());
        let t = mk_squash(mk_sum(inner.exposed.clone(), mk_mul([inner.term.clone(), matches])));
        let might_match = mk_mul(eqs.into_iter().zip(nulls.iter()).map(|(eq, null)| mk_add([eq, null.clone()])));
        let some_null = mk_squash(mk_add(nulls));
        let possible = mk_squash(mk_sum(inner.exposed, mk_mul([inner.term, might_match, some_null])));
        // FALSE iff no row matches and none might.
        Ok(Truth { f: mk_mul([mk_neg(t.clone()), mk_neg(possible)]), t })
    }

    /// Only numerically correct when at most one row of the inner query satisfies its own term (SQL
    /// raises an error otherwise). Java asserts that precondition globally, through a `ScalarTerm`
    /// constraint; this port does not model it yet.
    ///
    /// The value is NULL when no row satisfies the term *or* the one that does holds NULL in its
    /// column: under that precondition, exactly when no row holds a non-NULL value. Reading it as
    /// NULL only when there is no row would make `(SELECT y …) IS NULL` say `NOT EXISTS (…)`, never
    /// let a scalar aggregate (which always has its one row) be NULL, and turn `x = (SELECT …)`
    /// over a NULL value into FALSE where SQL says UNKNOWN -- which a `NOT` then makes TRUE.
    fn scalar_subquery_value(&mut self, query: &Relation, scope: &Scope) -> Result<Value, TranslateError> {
        let inner = self.rel(query, scope.base + scope.local.len())?;
        if inner.local.len() != 1 {
            return Err(TranslateError::ScalarSubqueryArity);
        }
        let col = UTerm::Var(inner.local[0].clone());
        let non_null_row = mk_squash(mk_sum(inner.exposed.clone(), mk_mul([inner.term.clone(), mk_neg(is_null_of(&col))])));
        let value = mk_sum(inner.exposed, mk_mul([inner.term, col]));
        Ok(Value { is_null: mk_neg(non_null_row), value })
    }

    fn translate_value(&mut self, e: &Expr, scope: &Scope) -> Result<Value, TranslateError> {
        match e {
            Expr::Column { index, .. } => Ok(column_value(scope.resolve(*index)?)),
            Expr::Literal { value, ty } => self.literal_value(value, *ty),
            Expr::Cast { ty, operand } => self.cast_value(*ty, operand, scope),
            Expr::Call { operator, operand, ty } => self.translate_call_value(operator, operand, *ty, scope),
            Expr::Subquery { operator, operand, query, .. } => match operator.as_str() {
                "EXISTS" => Ok(Value::not_null(self.exists_predicate(query, scope)?)),
                "IN" => {
                    let tr = self.in_truth(operand, query, scope)?;
                    Ok(Value { is_null: tr.u(), value: tr.t })
                }
                "$SCALAR_QUERY" => self.scalar_subquery_value(query, scope),
                other => Err(TranslateError::Subquery(other.to_string())),
            },
        }
    }

    /// Every cast is an uninterpreted function of its operand, one symbol per source and target
    /// type (`typed_symbol`), and null exactly when the operand is. Java erases every cast
    /// (`UExprConcreteTranslator`), which is a false-proof channel: `CAST(a AS REAL) / b` and
    /// `a / b` become the same term although integer division differs. An uninterpreted symbol
    /// cannot license a false proof, because the real conversion is one of its interpretations (the
    /// argument `src/casts.rs` makes for the frontend's own `qcastK`) -- provided one symbol never
    /// stands for two different conversions.
    ///
    /// Not even a cast between equal IR types is the identity. The frontend already drops the
    /// casts it knows compute nothing (`src/casts.rs` rule 4), so one that reaches the IR is one it
    /// did not: the IR's `INTEGER` also stands for DATE and TIMESTAMP, so `CAST(ts AS DATE)` over a
    /// timestamp arrives as INTEGER-to-INTEGER, and truncating to a day changes the value. The
    /// catch-all `VARBINARY` likewise does not record which real type it stands for, so the
    /// one-symbol-per-conversion proviso holds only as far as the IR carries the types. The source
    /// type is part of the name because the target alone does not fix the conversion: `CAST(1 AS
    /// TEXT)` is `'1'`, `CAST(1.0 AS TEXT)` is `'1.0'` and `CAST(TRUE AS TEXT)` is `'true'`.
    fn cast_value(&mut self, ty: Type, operand: &Expr, scope: &Scope) -> Result<Value, TranslateError> {
        let v = self.translate_value(operand, scope)?;
        let name = typed_symbol("cast", &[operand.ty()], ty);
        Ok(Value { is_null: v.is_null, value: UTerm::Func { name, args: vec![v.value] } })
    }

    fn translate_call_value(&mut self, operator: &str, operand: &[Expr], ty: Type, scope: &Scope) -> Result<Value, TranslateError> {
        if is_boolean_operator(operator, operand.len()) {
            // A boolean in value position is NULL exactly when it is UNKNOWN.
            let tr = self.truth_call(operator, operand, scope)?;
            return Ok(Value { is_null: tr.u(), value: tr.t });
        }
        match operator {
            "+" | "-" | "*" | "/" if operand.len() == 2 => {
                let a = self.translate_value(&operand[0], scope)?;
                let b = self.translate_value(&operand[1], scope)?;
                let is_null = mk_or(a.is_null, b.is_null);
                let value = match operator {
                    "+" => mk_add([a.value, b.value]),
                    "-" => mk_add([a.value, mk_mul([UTerm::Const(UConst::Int(-1)), b.value])]),
                    "*" => mk_mul([a.value, b.value]),
                    // Integer division truncates and decimal division does not, so `/` is one
                    // symbol per operand and result types (`typed_symbol`), never one for all.
                    _ => UTerm::Func { name: typed_symbol("divide", &types_of(operand), ty), args: vec![a.value, b.value] },
                };
                Ok(Value { is_null, value })
            }
            "+" if operand.len() == 1 => self.translate_value(&operand[0], scope),
            "-" if operand.len() == 1 => {
                let a = self.translate_value(&operand[0], scope)?;
                Ok(Value { is_null: a.is_null, value: mk_mul([UTerm::Const(UConst::Int(-1)), a.value]) })
            }
            "CASE" => self.case_value(operand, scope),
            "COALESCE" => self.coalesce_value(operand, scope),
            "NULLIF" if operand.len() == 2 => {
                let a = self.translate_value(&operand[0], scope)?;
                let b = self.translate_value(&operand[1], scope)?;
                let both_nonnull = mk_neg(mk_or(a.is_null.clone(), b.is_null.clone()));
                let eq = sql_eq(a.value.clone(), Some(operand[0].ty()), b.value, Some(operand[1].ty()));
                Ok(Value { is_null: mk_or(a.is_null, mk_mul([both_nonnull, eq])), value: a.value })
            }
            "||" if operand.len() == 2 => self.strict_value(&typed_symbol("concat", &types_of(operand), ty), operand, scope),
            _ if is_null_exactly_on_null_input(operator) => {
                self.strict_value(&typed_symbol(&operator.to_lowercase(), &types_of(operand), ty), operand, scope)
            }
            _ => self.opaque_value(&typed_symbol(operator, &types_of(operand), ty), operand, scope),
        }
    }

    /// A function known to be NULL exactly when some argument is NULL (strict, and total on non-null
    /// input). Its nullness is then derived, and its value is a function of the argument values.
    fn strict_value(&mut self, name: &str, operand: &[Expr], scope: &Scope) -> Result<Value, TranslateError> {
        let args: Vec<Value> = operand.iter().map(|e| self.translate_value(e, scope)).collect::<Result<_, _>>()?;
        let is_null = mk_squash(mk_add(args.iter().map(|a| a.is_null.clone())));
        let value = UTerm::Func { name: name.to_string(), args: args.into_iter().map(|a| a.value).collect() };
        Ok(Value { is_null, value })
    }

    /// Any other function: nothing is assumed about how it treats NULL, which rules out both
    /// "NULL in, NULL out" (false for `concat`, `greatest`, `concat_ws`, ...) and "non-NULL in,
    /// non-NULL out" (false for JSON extraction on a missing key). The function sees every argument
    /// as its (null flag, value) pair, and whether its result is NULL is a second uninterpreted
    /// symbol over the same arguments. Parameter carriers (`QPn`) land here too, since a parameter
    /// may be bound to NULL.
    fn opaque_value(&mut self, name: &str, operand: &[Expr], scope: &Scope) -> Result<Value, TranslateError> {
        let mut args = Vec::with_capacity(2 * operand.len());
        for e in operand {
            let v = self.translate_value(e, scope)?;
            args.push(v.is_null);
            args.push(v.value);
        }
        let is_null = truthy(UTerm::Func { name: format!("isnull:{name}"), args: args.clone() });
        Ok(Value { is_null, value: UTerm::Func { name: name.to_string(), args } })
    }

    /// `operand` is Calcite's flattened `[cond, then, cond, then, ..., else]`, always odd-length (a
    /// trailing `ELSE` is always present). Guards accumulate the negation of every prior condition so
    /// exactly one branch's guard is 1 for any input, making a plain sum over `guard * branch` correct.
    fn case_value(&mut self, operand: &[Expr], scope: &Scope) -> Result<Value, TranslateError> {
        if operand.is_empty() || operand.len() % 2 == 0 {
            return Err(TranslateError::MalformedShape("CASE operand must be [cond, then, ..., else]".into()));
        }
        let n = (operand.len() - 1) / 2;
        let mut remaining = UTerm::Const(UConst::Int(1));
        let mut value_terms = Vec::with_capacity(n + 1);
        let mut null_terms = Vec::with_capacity(n + 1);
        for i in 0..n {
            let cond = self.translate_predicate(&operand[2 * i], scope)?;
            let then = self.translate_value(&operand[2 * i + 1], scope)?;
            let guard = mk_mul([remaining.clone(), cond.clone()]);
            value_terms.push(mk_mul([guard.clone(), then.value]));
            null_terms.push(mk_mul([guard, then.is_null]));
            remaining = mk_mul([remaining, mk_neg(cond)]);
        }
        let else_val = self.translate_value(&operand[operand.len() - 1], scope)?;
        value_terms.push(mk_mul([remaining.clone(), else_val.value]));
        null_terms.push(mk_mul([remaining, else_val.is_null]));
        Ok(Value { is_null: mk_add(null_terms), value: mk_add(value_terms) })
    }

    /// First non-null operand wins; null only if every operand is null.
    fn coalesce_value(&mut self, operand: &[Expr], scope: &Scope) -> Result<Value, TranslateError> {
        if operand.is_empty() {
            return Err(TranslateError::MalformedShape("COALESCE needs at least one operand".into()));
        }
        let mut remaining = UTerm::Const(UConst::Int(1));
        let mut value_terms = Vec::with_capacity(operand.len());
        for e in operand {
            let v = self.translate_value(e, scope)?;
            let guard = mk_mul([remaining.clone(), mk_neg(v.is_null.clone())]);
            value_terms.push(mk_mul([guard, v.value]));
            remaining = mk_mul([remaining, v.is_null]);
        }
        Ok(Value { is_null: remaining, value: mk_add(value_terms) })
    }

    /// A literal as the constant it denotes, exactly, or a refusal (`literal:<type>`) when its text
    /// does not denote one of its type: an INTEGER literal must be an `i64` (`1e-5` or a number past
    /// the `bigint` range is not, and reading it through a float would make it `0` or `i64::MAX`), a
    /// REAL literal a decimal number, a BOOLEAN literal `true` or `false`. A REAL literal stays a
    /// decimal even when integral (`2.0` is not `2`), and a BOOLEAN one is the 0/1 a predicate in
    /// value position is; booleans and integers never meet in one position, since no Postgres
    /// operator or function takes either for the other and every symbol is named by its operand
    /// types, and `prove` compares the two sides' output types.
    fn literal_value(&self, value: &str, ty: Type) -> Result<Value, TranslateError> {
        if value == "NULL" {
            return Ok(Value::null());
        }
        let refuse = || TranslateError::Literal(ty.name().to_string());
        let term = match ty {
            Type::Integer => UTerm::Const(UConst::Int(value.parse::<i64>().map_err(|_| refuse())?)),
            Type::Boolean => match value.to_ascii_lowercase().as_str() {
                "true" => UTerm::Const(UConst::Int(1)),
                "false" => UTerm::Const(UConst::Int(0)),
                _ => return Err(refuse()),
            },
            Type::Real => UTerm::Const(UConst::decimal(value).ok_or_else(refuse)?),
            Type::Varchar | Type::Varbinary => UTerm::Const(UConst::Str(value.to_string())),
            // Refused by `ir::Expr::literal` before translation; keyed by type anyway, so that a
            // date and a timestamp spelled alike could never be one constant.
            Type::Date | Type::Time | Type::Timestamp | Type::Interval => {
                UTerm::Const(UConst::Str(format!("{}:{value}", ty.name())))
            }
        };
        Ok(Value::not_null(term))
    }
}

enum SetOpKind {
    Union,
    Except,
    Intersect,
}

/// One set-op branch's contribution, reprojected onto the shared `out_local` vars so both sides speak
/// about the same output identity before being combined by the caller.
fn set_side_term(term: UTerm, exposed: Vec<UVar>, side_local: &[UVar], out_local: &[UVar]) -> UTerm {
    let col_eq = mk_mul(side_local.iter().zip(out_local.iter()).map(|(sv, ov)| UTerm::Pred {
        kind: PredKind::Eq,
        args: vec![UTerm::Var(sv.clone()), UTerm::Var(ov.clone())],
    }));
    mk_sum(exposed, mk_mul([term, col_eq]))
}

/// Id of the output tuple var. Both sides are closed over this same free var, so their terms are
/// functions of one shared variable and can be compared directly (Java's `alignOutVar`,
/// `UExprConcreteTranslator.java:178-187`). `fresh_var` counts up from 0 and never reaches it.
pub const OUT_VAR_ID: u32 = u32::MAX;

/// One side, closed: `term` is a function of `Base(OUT_VAR_ID)` alone (every other var is bound by a
/// `Sum`), and that var has exactly `arity` columns, `Proj(0..arity)`. `widths` gives the column
/// count of every base var the term mentions, the output var included, and `types` the IR type of
/// each output column ([`Relation::output_types`]), which the term itself does not carry.
#[derive(Debug, Clone)]
pub struct Query {
    pub term: UTerm,
    pub arity: usize,
    pub widths: std::collections::HashMap<u32, usize>,
    pub types: Vec<Option<Type>>,
}

/// Closes a top-level translation over the shared output var. When the relation already exposes a
/// single var whose columns are exactly its output (every operator but `Join` mints one), that var
/// simply becomes the output var. Otherwise -- a top-level join, whose output is the concatenation
/// of several vars -- the exposed vars are summed out and bound column-by-column to the output var.
/// The binding is identity (`Null` equals `Null`), not SQL `=`, so null output columns are kept.
fn close_output(t: Translated, widths: Vec<usize>, types: Vec<Option<Type>>) -> Query {
    let arity = t.local.len();
    let mut widths: std::collections::HashMap<u32, usize> =
        widths.into_iter().enumerate().map(|(id, w)| (id as u32, w)).collect();
    widths.insert(OUT_VAR_ID, arity);
    if let [v @ UVar::Base(id)] = t.exposed.as_slice() {
        if t.local.iter().enumerate().all(|(i, l)| *l == UVar::proj(i as u32, v.clone())) {
            return Query { term: t.term.rename_base(*id, OUT_VAR_ID), arity, widths, types };
        }
    }
    let out = UVar::Base(OUT_VAR_ID);
    let bind = mk_mul(t.local.iter().enumerate().map(|(i, l)| UTerm::Pred {
        kind: PredKind::Eq,
        args: vec![UTerm::Var(UVar::proj(i as u32, out.clone())), UTerm::Var(l.clone())],
    }));
    Query { term: mk_sum(t.exposed, mk_mul([t.term, bind])), arity, widths, types }
}

/// Translates both sides of an `Input` independently -- each gets its own freshly-seeded `Translator`
/// (var numbering starting at 0), since the two sides are never compared by raw var identity, only by
/// alpha-equivalence, which treats bound vars as fully interchangeable. The one var they do
/// share is the output var (see `close_output`).
pub fn translate_input(input: &crate::ir::Input) -> Result<[Query; 2], TranslateError> {
    let types = |q: &Relation| q.output_types(&input.schemas);
    let mut left = Translator::new(&input.schemas);
    let l = left.rel(&input.queries[0], 0)?;
    let mut right = Translator::new(&input.schemas);
    let r = right.rel(&input.queries[1], 0)?;
    Ok([
        close_output(l, left.widths, types(&input.queries[0])),
        close_output(r, right.widths, types(&input.queries[1])),
    ])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::eval::{Db, Env};
    use crate::ir::{Input, Schema};
    use serde_json::json;

    fn one_table(types: Vec<Type>) -> Vec<Schema> {
        vec![Schema { name: "t0".to_string(), types, key: vec![], nullable: vec![] }]
    }

    #[test]
    fn a_join_right_input_numbers_its_own_columns_from_the_enclosing_base() {
        // `s JOIN (SELECT * FROM t WHERE t.x = 1) AS t ON s.id = t.x`, as the frontend lowers it:
        // the derived table's filter names `t.x` as column 0, from the enclosing base, not as
        // column 2 after `s`; the join condition sees `s ++ t`.
        let schemas = vec![
            Schema { name: "s".to_string(), types: vec![Type::Integer, Type::Integer], key: vec![], nullable: vec![] },
            Schema { name: "t".to_string(), types: vec![Type::Integer], key: vec![], nullable: vec![] },
        ];
        let col = |index| Expr::Column { index, ty: Type::Integer };
        let eq = |a, b| Expr::Call { operator: "=".to_string(), ty: Type::Boolean, operand: vec![a, b] };
        let one = Expr::Literal { value: "1".to_string(), ty: Type::Integer };
        let right = Relation::Filter { source: Box::new(Relation::Scan(1)), condition: eq(col(0), one) };
        let join = Relation::Join {
            left: Box::new(Relation::Scan(0)),
            right: Box::new(right),
            kind: JoinKind::Inner,
            condition: eq(col(0), col(2)),
        };
        let joined = Translator::new(&schemas).rel(&join, 0).expect("not correlated");
        assert_eq!(joined.local.len(), 3);
    }

    /// Every var `t` reads as a value (not the tuple vars of table atoms or binders).
    fn value_vars(t: &UTerm, out: &mut Vec<UVar>) {
        match t {
            UTerm::Var(v) => out.push(v.clone()),
            UTerm::Const(_) | UTerm::Table { .. } => {}
            UTerm::Pred { args, .. } | UTerm::Func { args, .. } => args.iter().for_each(|a| value_vars(a, out)),
            UTerm::Add(ts) | UTerm::Mul(ts) => ts.iter().for_each(|c| value_vars(c, out)),
            UTerm::Squash(c) | UTerm::Neg(c) => value_vars(c, out),
            UTerm::Sum { body, .. } => value_vars(body, out),
        }
    }

    #[test]
    fn a_join_right_input_column_past_the_left_width_is_its_own() {
        // `s JOIN (SELECT * FROM t WHERE t.c = 1) AS d ON s.id = t.a` with `s(id)` and `t(a, b, c)`:
        // the filter's `t.c` is column 2 from the enclosing base. Numbering the right input from
        // after `s` would not refuse this one: it would read column 2 as `t.b`.
        let schemas = vec![
            Schema { name: "s".to_string(), types: vec![Type::Integer], key: vec![], nullable: vec![] },
            Schema { name: "t".to_string(), types: vec![Type::Integer; 3], key: vec![], nullable: vec![] },
        ];
        let col = |index| Expr::Column { index, ty: Type::Integer };
        let eq = |a, b| Expr::Call { operator: "=".to_string(), ty: Type::Boolean, operand: vec![a, b] };
        let one = Expr::Literal { value: "1".to_string(), ty: Type::Integer };
        let right = Relation::Filter { source: Box::new(Relation::Scan(1)), condition: eq(col(2), one) };
        let join = Relation::Join {
            left: Box::new(Relation::Scan(0)),
            right: Box::new(right),
            kind: JoinKind::Inner,
            condition: eq(col(0), col(1)),
        };
        let joined = Translator::new(&schemas).rel(&join, 0).expect("translates");
        let mut read = Vec::new();
        value_vars(&joined.term, &mut read);
        // The joined row is `s.id, t.a, t.b, t.c`.
        assert!(read.contains(&joined.local[3]), "the filter reads t.c");
        assert!(!read.contains(&joined.local[2]), "nothing reads t.b");
    }

    #[test]
    fn scan_produces_a_table_indicator_over_a_fresh_base_var() {
        let schemas = one_table(vec![Type::Integer, Type::Varchar]);
        let mut t = Translator::new(&schemas);
        let scanned = t.scan(0).unwrap();
        assert!(matches!(scanned.term, UTerm::Table { .. }));
        assert_eq!(scanned.local.len(), 2);
        assert_eq!(scanned.exposed.len(), 1);
    }

    #[test]
    fn scalar_aggregation_forces_group_presence_even_with_no_rows() {
        let schemas = one_table(vec![Type::Integer]);
        let mut t = Translator::new(&schemas);
        let count_star = AggCall { operator: "COUNT".to_string(), ty: Type::Integer, distinct: false, operand: vec![] };
        let grouped = t.group(&Relation::Scan(0), &[], std::slice::from_ref(&count_star), 0).unwrap();
        // The group's overall indicator is `Mul([key_presence, ...agg_eqs])`; with no GROUP BY keys,
        // `key_presence` must be the unconditional `Const(1)` (not a sum-based existence check, which
        // would wrongly vanish over an empty input) so scalar aggregation still yields one row.
        match &grouped.term {
            UTerm::Mul(factors) => assert_eq!(*factors[0], UTerm::Const(UConst::Int(1))),
            other => panic!("expected Mul([key_presence, ...]), got {other:?}"),
        }
    }

    #[test]
    fn is_null_predicate_on_a_column_matches_the_sentinel_equality() {
        let schemas = one_table(vec![Type::Integer]);
        let mut t = Translator::new(&schemas);
        let src = t.scan(0).unwrap();
        let scope = Scope { base: 0, local: src.local.clone() };
        let e = Expr::Call {
            operator: "IS NULL".to_string(),
            ty: Type::Boolean,
            operand: vec![Expr::Column { index: 0, ty: Type::Integer }],
        };
        let got = t.translate_predicate(&e, &scope).unwrap();
        let want = is_null_of(&UTerm::Var(src.local[0].clone()));
        assert_eq!(got, want);
    }

    #[test]
    fn translate_input_round_trips_a_trivial_equivalence_query() {
        let query = json!({ "project": {
            "target": [{ "column": 0, "type": "INTEGER" }],
            "source": { "scan": 0 },
        }});
        let input_json = json!({
            "schemas": [{ "types": ["INTEGER"], "key": [], "nullable": [true] }],
            "queries": [query.clone(), query],
        });
        let input = Input::parse(&input_json).unwrap();
        let [l, r] = translate_input(&input).unwrap();
        assert_eq!(l.term, r.term);
        assert_eq!((l.arity, r.arity), (1, 1));
    }

    fn two_col_schema() -> serde_json::Value {
        json!([{ "types": ["INTEGER", "INTEGER"], "key": [], "nullable": [true, true] }])
    }

    #[test]
    fn a_scan_rooted_side_uses_its_own_var_as_the_output_var() {
        let input = Input::parse(&json!({
            "schemas": two_col_schema(),
            "queries": [{ "scan": 0 }, { "scan": 0 }],
        }))
        .unwrap();
        let [l, _] = translate_input(&input).unwrap();
        assert_eq!(l.term, UTerm::Table { name: "t0".to_string(), var: UVar::Base(OUT_VAR_ID) });
        assert_eq!(l.arity, 2);
    }

    #[test]
    fn a_join_rooted_side_sums_out_its_vars_and_binds_every_column() {
        let join = json!({ "join": {
            "kind": "INNER",
            "left": { "scan": 0 },
            "right": { "scan": 0 },
            "condition": { "operator": "true", "operand": [], "type": "BOOLEAN" },
        }});
        let input = Input::parse(&json!({ "schemas": two_col_schema(), "queries": [join.clone(), join] })).unwrap();
        let [l, _] = translate_input(&input).unwrap();
        assert_eq!(l.arity, 4);
        let UTerm::Sum { vars, body } = &l.term else { panic!("expected a Sum, got {:?}", l.term) };
        assert_eq!(vars, &vec![UVar::Base(0), UVar::Base(1)]);
        // All four output columns are bound, each to exactly one source column.
        let bound: Vec<&UTerm> = match &**body {
            UTerm::Mul(fs) => fs.iter().map(|f| &**f).filter(|f| matches!(f, UTerm::Pred { .. })).collect(),
            other => panic!("expected a Mul body, got {other:?}"),
        };
        for i in 0..4u32 {
            let out_col = UTerm::Var(UVar::proj(i, UVar::Base(OUT_VAR_ID)));
            assert_eq!(bound.iter().filter(|p| matches!(p, UTerm::Pred { args, .. } if args[0] == out_col)).count(), 1);
        }
    }

    /// SQL's own evaluation of the small integer expression language the truth-table test uses:
    /// `None` is NULL for a value and UNKNOWN for a boolean.
    fn sql_value(e: &Expr, row: &[UConst]) -> Option<i64> {
        match e {
            Expr::Column { index, .. } => match &row[*index as usize] {
                UConst::Int(n) => Some(*n),
                _ => None,
            },
            Expr::Literal { value, .. } => value.parse().ok(),
            other => panic!("unexpected value expression {other:?}"),
        }
    }

    fn sql_truth(e: &Expr, row: &[UConst]) -> Option<bool> {
        let Expr::Call { operator, operand, .. } = e else { panic!("unexpected predicate {e:?}") };
        let truths = || operand.iter().map(|o| sql_truth(o, row)).collect::<Vec<_>>();
        match operator.as_str() {
            "AND" if truths().contains(&Some(false)) => Some(false),
            "AND" => truths().iter().all(|t| *t == Some(true)).then_some(true),
            "OR" if truths().contains(&Some(true)) => Some(true),
            "OR" => truths().iter().all(|t| *t == Some(false)).then_some(false),
            "NOT" => sql_truth(&operand[0], row).map(|b| !b),
            "IS NULL" => Some(sql_value(&operand[0], row).is_none()),
            "IS NOT NULL" => Some(sql_value(&operand[0], row).is_some()),
            "IS NOT TRUE" => Some(sql_truth(&operand[0], row) != Some(true)),
            "IS DISTINCT FROM" => Some(sql_value(&operand[0], row) != sql_value(&operand[1], row)),
            op => {
                let (a, b) = (sql_value(&operand[0], row)?, sql_value(&operand[1], row)?);
                Some(match op {
                    "=" => a == b,
                    "<>" => a != b,
                    "<" => a < b,
                    ">" => a > b,
                    other => panic!("unexpected operator {other}"),
                })
            }
        }
    }

    fn call(op: &str, operand: Vec<serde_json::Value>) -> serde_json::Value {
        json!({ "operator": op, "type": "BOOLEAN", "operand": operand })
    }

    fn c(i: u32) -> serde_json::Value {
        json!({ "column": i, "type": "INTEGER" })
    }

    fn lit(n: i64) -> serde_json::Value {
        json!({ "operator": n.to_string(), "operand": [], "type": "INTEGER" })
    }

    #[test]
    fn predicates_follow_sql_three_valued_logic() {
        let eq01 = call("=", vec![c(0), lit(1)]);
        let eq12 = call("=", vec![c(1), lit(2)]);
        let exprs = [
            call("NOT", vec![eq01.clone()]),
            call("NOT", vec![call("AND", vec![eq01.clone(), eq12.clone()])]),
            call("NOT", vec![call("OR", vec![eq01.clone(), eq12.clone()])]),
            call("NOT", vec![call("NOT", vec![eq01.clone()])]),
            call("OR", vec![eq01.clone(), call("IS NULL", vec![c(1)])]),
            call("NOT", vec![call("IS DISTINCT FROM", vec![c(0), c(1)])]),
            call("IS NOT TRUE", vec![eq01.clone()]),
            call("NOT", vec![call("IS NOT TRUE", vec![call("<", vec![c(0), c(1)])])]),
            call("OR", vec![call("AND", vec![eq01.clone(), call(">", vec![c(1), lit(1)])]), call("NOT", vec![call("<>", vec![c(1), lit(2)])])]),
            call("NOT", vec![call("AND", vec![eq01, call("NOT", vec![call("OR", vec![call("=", vec![c(1), lit(1)]), call("IS NOT NULL", vec![c(0)])])])])]),
        ];
        let schemas = one_table(vec![Type::Integer, Type::Integer]);
        let values = [UConst::Null, UConst::Int(1), UConst::Int(2)];
        for json in &exprs {
            let e = Expr::parse(json, 1).unwrap();
            let mut t = Translator::new(&schemas);
            let src = t.scan(0).unwrap();
            let truth = t.translate_truth(&e, &Scope { base: 0, local: src.local }).unwrap();
            let db = Db::new(Default::default(), values.to_vec(), Default::default());
            for a in &values {
                for b in &values {
                    let row = vec![a.clone(), b.clone()];
                    let mut env = Env::from([(0, row.clone())]);
                    let mut count = |t: &UTerm| db.count(t, &mut env).unwrap();
                    let got = (count(&truth.t), count(&truth.u()), count(&truth.f));
                    let want = match sql_truth(&e, &row) {
                        Some(true) => (1, 0, 0),
                        Some(false) => (0, 0, 1),
                        None => (0, 1, 0),
                    };
                    assert_eq!(got, want, "{json} on row {row:?}");
                }
            }
        }
    }

    #[test]
    fn in_is_unknown_when_only_a_null_could_match() {
        let schemas = vec![
            Schema { name: "t".into(), types: vec![Type::Integer], key: vec![], nullable: vec![] },
            Schema { name: "s".into(), types: vec![Type::Integer], key: vec![], nullable: vec![] },
        ];
        let e = Expr::Subquery {
            operator: "IN".into(),
            ty: Type::Boolean,
            operand: vec![Expr::Column { index: 0, ty: Type::Integer }],
            query: Box::new(Relation::Scan(1)),
        };
        let mut t = Translator::new(&schemas);
        let src = t.scan(0).unwrap();
        let truth = t.translate_truth(&e, &Scope { base: 0, local: src.local }).unwrap();
        let widths = t.widths.iter().enumerate().map(|(i, w)| (i as u32, *w)).collect();
        let (n, one, two, three) = (UConst::Null, UConst::Int(1), UConst::Int(2), UConst::Int(3));
        // (contents of s, the operand, expected (TRUE, UNKNOWN))
        let cases = [
            (vec![one.clone(), n.clone()], one.clone(), (1, 0)),
            (vec![one.clone(), n.clone()], two.clone(), (0, 1)),
            (vec![one.clone(), n.clone()], n.clone(), (0, 1)),
            (vec![one.clone(), two.clone()], three.clone(), (0, 0)),
            (vec![one.clone(), two.clone()], n.clone(), (0, 1)),
            (vec![], n.clone(), (0, 0)),
            (vec![n.clone()], one.clone(), (0, 1)),
        ];
        for (s, x, want) in cases {
            let db = Db::new(
                [("s".to_string(), s.iter().map(|v| vec![v.clone()]).collect())].into(),
                vec![n.clone(), one.clone(), two.clone(), three.clone()],
                std::collections::HashMap::clone(&widths),
            );
            let mut env = Env::from([(0, vec![x.clone()])]);
            let mut count = |t: &UTerm| db.count(t, &mut env).unwrap();
            let got = (count(&truth.t), count(&truth.u()), count(&truth.f));
            let want = (want.0, want.1, 1 - want.0 - want.1);
            assert_eq!(got, want, "{x:?} IN {s:?}");
        }
    }

    #[test]
    fn aggregate_output_columns_are_dense() {
        let schemas = one_table(vec![Type::Integer, Type::Integer]);
        let mut t = Translator::new(&schemas);
        let count = |operand: Vec<Expr>| AggCall { operator: "COUNT".to_string(), ty: Type::Integer, distinct: false, operand };
        let keys = [Expr::Column { index: 0, ty: Type::Integer }];
        let col1 = Expr::Column { index: 1, ty: Type::Integer };
        let grouped = t.group(&Relation::Scan(0), &keys, &[count(vec![]), count(vec![col1]), count(vec![])], 0).unwrap();
        let out = grouped.exposed[0].clone();
        let want: Vec<UVar> = (0..4).map(|i| UVar::proj(i, out.clone())).collect();
        assert_eq!(grouped.local, want);
    }

    #[test]
    fn union_requires_matching_column_counts() {
        let schemas = one_table(vec![Type::Integer, Type::Varchar]);
        let mut t = Translator::new(&schemas);
        let wide = Relation::Scan(0);
        let narrow = Relation::Project { source: Box::new(Relation::Scan(0)), target: vec![Expr::Column { index: 0, ty: Type::Integer }] };
        let sides = [Box::new(wide), Box::new(narrow)];
        let err = t.set_op(SetOpKind::Union, &sides, 0).unwrap_err();
        assert_eq!(err, TranslateError::MalformedShape("set-op branches have different column counts".into()));
    }

    /// A scalar subquery's value is NULL when it has no row or its row holds NULL: checked on
    /// concrete databases, against what Postgres answers.
    mod scalar_subquery_nullness {
        use super::*;

        /// `t(id)`, `s(k, y)`, and the truth of `cond` on `t`'s one row `(1)` for each content of
        /// `s`: (TRUE, UNKNOWN).
        fn truths(cond: serde_json::Value, contents: &[Vec<UConst>]) -> Vec<(i64, i64)> {
            let schemas = vec![
                Schema { name: "t".into(), types: vec![Type::Integer], key: vec![], nullable: vec![] },
                Schema { name: "s".into(), types: vec![Type::Integer, Type::Integer], key: vec![], nullable: vec![] },
            ];
            let e = Expr::parse(&cond, 2).unwrap();
            let mut t = Translator::new(&schemas);
            let src = t.scan(0).unwrap();
            let truth = t.translate_truth(&e, &Scope { base: 0, local: src.local }).unwrap();
            let widths: std::collections::HashMap<u32, usize> = t.widths.iter().enumerate().map(|(i, w)| (i as u32, *w)).collect();
            let universe = vec![UConst::Null, UConst::Int(0), UConst::Int(1), UConst::Int(2), UConst::Int(5)];
            contents
                .iter()
                .map(|rows| {
                    let s = rows.chunks(2).map(|r| r.to_vec()).collect();
                    let db = Db::new([("s".to_string(), s)].into(), universe.clone(), widths.clone());
                    let mut env = Env::from([(0, vec![UConst::Int(1)])]);
                    let mut count = |t: &UTerm| db.count(t, &mut env).unwrap();
                    (count(&truth.t), count(&truth.u()))
                })
                .collect()
        }

        fn scalar(query: serde_json::Value) -> serde_json::Value {
            json!({ "operator": "$SCALAR_QUERY", "type": "INTEGER", "operand": [], "query": query })
        }

        /// `SELECT y FROM s WHERE k = 1`.
        fn y_where_k_is_1() -> serde_json::Value {
            json!({ "project": { "source": { "filter": { "source": { "scan": 1 }, "condition": call("=", vec![c(1), lit(1)]) } },
                                 "target": [c(2)] } })
        }

        /// `SELECT sum(y) FROM s`.
        fn sum_y() -> serde_json::Value {
            json!({ "group": { "keys": [], "source": { "project": { "source": { "scan": 1 }, "target": [c(2)] } },
                               "function": [{ "operator": "SUM", "type": "INTEGER", "operand": [c(1)] }] } })
        }

        #[test]
        fn is_null_holds_for_no_row_and_for_a_row_holding_null() {
            let (n, one, two, five) = (UConst::Null, UConst::Int(1), UConst::Int(2), UConst::Int(5));
            let contents = [vec![], vec![one.clone(), n.clone()], vec![one.clone(), five.clone()], vec![two.clone(), five.clone()]];
            let got = truths(call("IS NULL", vec![scalar(y_where_k_is_1())]), &contents);
            assert_eq!(got, [(1, 0), (1, 0), (0, 0), (1, 0)]);
            // sum over no rows, or over only NULLs, is NULL.
            let got = truths(call("IS NULL", vec![scalar(sum_y())]), &contents[..3]);
            assert_eq!(got, [(1, 0), (1, 0), (0, 0)]);
        }

        #[test]
        fn a_comparison_with_a_null_subquery_is_unknown_and_stays_so_under_not() {
            let (n, one, five) = (UConst::Null, UConst::Int(1), UConst::Int(5));
            let contents = [vec![], vec![one.clone(), n.clone()], vec![one.clone(), five.clone()], vec![one.clone(), one.clone()]];
            let eq = call("=", vec![c(0), scalar(sum_y())]);
            assert_eq!(truths(eq.clone(), &contents), [(0, 1), (0, 1), (0, 0), (1, 0)]);
            assert_eq!(truths(call("NOT", vec![eq]), &contents), [(0, 1), (0, 1), (1, 0), (0, 0)]);
        }
    }
}
