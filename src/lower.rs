// Unless explicitly stated otherwise all files in this repository are licensed under the
// Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/).
// Copyright 2026-Present Datadog, Inc.

//! Lowering: SQL AST -> the prover's `Relation`/`Expr` IR (as `serde_json::Value`).
//!
//! Columns are lowered to absolute de-Bruijn indices (see [`crate::scope`]). Every clause that would
//! change the result and that we cannot lower faithfully is *refused* (returns `Err`) rather than
//! lowered best-effort — this is what preserves soundness end-to-end.

use std::collections::HashMap;
use std::ops::ControlFlow;

use serde_json::{json, Value};
use sqlparser::ast::visit_expressions;
use sqlparser::ast::{
    BinaryOperator, Distinct, DuplicateTreatment, Expr, Function, FunctionArg, FunctionArgExpr,
    FunctionArgumentClause, FunctionArguments, GroupByExpr, JoinConstraint, JoinOperator, OrderBy,
    Query,
    Select, SelectItem, SelectItemQualifiedWildcardKind, SetExpr, SetOperator, SetQuantifier,
    TableFactor, TableWithJoins, UnaryOperator, Value as SqlValue, Values,
};

use crate::catalog::{obj_name, Catalog, FnDecl};
use crate::error::{schema, unsupported, Result};
use crate::scope::{Binding, Scope};
use crate::types::*;

/// Output columns of a (sub)query: `(name, prover type)`.
type OutCols = Vec<(String, String)>;

/// Functions declared by the `declare ... function` DSL, keyed by uppercased name.
type Fns = HashMap<String, FnDecl>;

/// The aggregates the prover models natively, and whose SQL semantics skip NULL inputs.
///
/// Both halves of that sentence are load-bearing and neither is negotiable, which is why
/// [`OPAQUE_AGGS`] is a separate list rather than more entries here: `ignoreNulls` and the `FILTER`
/// rewrite are both gated on membership, and both are only correct for a null-skipping aggregate the
/// prover actually interprets.
const BUILTIN_AGGS: [&str; 5] = ["COUNT", "SUM", "AVG", "MIN", "MAX"];

/// Aggregates the prover does not model, lowered as *uninterpreted aggregate* symbols — exactly what
/// a `declare aggregate function` line produces, without needing the line — paired with the type
/// they return.
///
/// Without this, `bool_or` matches nothing: it is not in [`BUILTIN_AGGS`] and nobody declared it, so
/// [`is_agg_call`] says no, the query never takes the Group path, and the call is lowered as an
/// ordinary per-row scalar. That silently turns one output row into one row per input row — a
/// cardinality mis-lowering of the same class as [`SET_RETURNING`], reached from the other side.
///
/// The demonstrated effect is lost coverage: `SELECT DISTINCT bool_or(b) FROM t` against
/// `SELECT bool_or(b) FROM t` is the same one row and now proves, where the demoted form compared a
/// DISTINCT over N rows against N rows and could not. No false-proof witness was found for the
/// demotion, which is why this is not filed as a soundness fix — but getting the cardinality of an
/// aggregate wrong is not a thing to leave standing on the strength of having failed to exploit it.
///
/// The preprocessor refuses these instead, for a reason that does not apply here: sqlglot parses them
/// into dedicated node types that render as a plain call, so its downstream had no way to say
/// "aggregate". Nothing stops us saying it.
///
/// The return types are definitional rather than guessed — `bool_or`/`bool_and`/`every` return a
/// boolean in every dialect that has them — so stating them is not the kind of assumption
/// [`UNDECLARED_RET`] is careful about. Null-handling is *not* asserted: `ignoreNulls` stays `false`
/// and `FILTER` stays refused, both of which are the incomplete-not-unsound direction.
const OPAQUE_AGGS: [(&str, &str); 3] =
    [("BOOL_OR", "BOOLEAN"), ("BOOL_AND", "BOOLEAN"), ("EVERY", "BOOLEAN")];

/// Aggregates whose result is not determined by the bag of values they fold over.
///
/// SOUNDNESS GUARD, and the reason [`OPAQUE_AGGS`] cannot simply be extended with them.
///
/// An uninterpreted aggregate symbol is a *function of the bag*: the prover assumes nothing about
/// what it computes, but it does assume that feeding it equal bags gives equal results. That is what
/// makes `bool_or` safe up there — disjunction is commutative, associative and idempotent, so the bag
/// really does determine the answer.
///
/// These do not have that property. `array_agg(v)` over the bag `{a, b}` is `[a, b]` or `[b, a]`
/// depending on the order rows reach the aggregate, which SQL leaves unspecified and which the plan
/// decides; `string_agg` and the `json*_agg` family are the same. Modelling one as a function of the
/// bag therefore asserts an equality Postgres does not honour — and asserting *more* equalities than
/// reality is the unsound direction, because a rewrite that preserves the bag while changing the
/// order (join reordering, a different index) would come out provably equivalent when it is not.
///
/// The alternative is not "model them anyway": within bag semantics, where a relation has no order at
/// all, an order-sensitive aggregate is simply not expressible. Refusing is how that is said.
///
/// Refusing also fixes a second, independent defect these had while they were unlisted. Being absent
/// from both aggregate lists, [`is_agg_call`] said no, the query never took the Group path, and
/// `SELECT array_agg(v) FROM t` lowered to a *per-row scalar* over the scan — N output rows where SQL
/// returns exactly one. That is the [`OPAQUE_AGGS`] cardinality bug over again, and it was live: four
/// corpus cases lowered that way and all four proved. None of the four is a false verdict — each is a
/// `DISTINCT`-inside-`IN` rewrite that `normalize::strip_in_exists_distinct` makes reflexive, so both
/// sides carried the identical wrong shape and the theorem was `X ≡ X` — but that is luck, not a
/// property, and those four are exactly the reflexive pairs that measure no capability.
///
/// The preprocessor demotes these to `qa_*` aggregate symbols instead, which fixes the cardinality
/// and takes on the bag-determinism assumption. That is the trade being declined here.
const ORDER_SENSITIVE_AGGS: [&str; 9] = [
    "ARRAY_AGG",
    "STRING_AGG",
    "GROUP_CONCAT",
    "LISTAGG",
    "JSON_AGG",
    "JSONB_AGG",
    "JSON_OBJECT_AGG",
    "JSONB_OBJECT_AGG",
    "XMLAGG",
];

/// Functions whose result can differ between two calls with the same arguments.
///
/// SOUNDNESS GUARD. Every other unknown call is modelled as an uninterpreted *function*, and the
/// whole force of that word is that equal arguments give equal results — which is what lets both
/// sides of a rewrite share one symbol. These do not have that property: `random()` twice is two
/// values, `nextval` advances a sequence, `clock_timestamp()` moves during the statement. Modelling
/// one as a function asserts an equality the database does not honour, so a pair that differs only
/// in how many times it calls one would come out equivalent.
///
/// Not to be confused with the statement-stable clocks — `now()`, `current_timestamp`,
/// `transaction_timestamp()`, `localtimestamp` — which are fixed for the duration of a statement and
/// so *are* faithful as shared constants. They are deliberately absent from this list.
const NONDETERMINISTIC: [&str; 10] = [
    "RANDOM",
    "GEN_RANDOM_UUID",
    "UUID_GENERATE_V1",
    "UUID_GENERATE_V4",
    "UUID",
    "RANDOM_UUID",
    "NEXTVAL",
    "CURRVAL",
    "SETVAL",
    "CLOCK_TIMESTAMP",
];

/// Set-returning functions: the ones that expand one input row into *many* output rows.
///
/// Every other unknown function is lowered as an uninterpreted scalar, which is faithful because a
/// function is a function — whatever it computes, it computes one value and both queries get the
/// same symbol. These break that: `SELECT EXPLODE(a) FROM t` returns one row per array element, not
/// one per input row, so modelling the call as a scalar understates the cardinality. That is
/// invisible when both sides use it identically but not otherwise (`SELECT DISTINCT EXPLODE(a)`
/// against `SELECT EXPLODE(a)` would come out equal on a one-row table and are not), so they are
/// refused in scalar position rather than mis-modelled.
const SET_RETURNING: [&str; 14] = [
    "EXPLODE",
    "EXPLODE_OUTER",
    "POSEXPLODE",
    "POSEXPLODE_OUTER",
    "INLINE",
    "INLINE_OUTER",
    "UNNEST",
    "GENERATE_SERIES",
    "GENERATE_SUBSCRIPTS",
    "JSON_ARRAY_ELEMENTS",
    "JSON_ARRAY_ELEMENTS_TEXT",
    "JSONB_ARRAY_ELEMENTS",
    "JSONB_ARRAY_ELEMENTS_TEXT",
    "REGEXP_SPLIT_TO_TABLE",
];

/// The result type assumed for a call nobody declared.
///
/// `VARBINARY` is opaque here ([`is_builtin`] is false for it), so [`common_type`] lets it win a
/// coercion and [`make_arith`] emits an *uninterpreted* `+` over an uninterpreted sort. `INTEGER`
/// — what this used to be — instead hands the prover genuine integer arithmetic and a total order.
///
/// The opaque choice is the faithful one whichever type the function really returns. If it really
/// returns an integer, the uninterpreted reading is an abstraction of the integer one: every real
/// behaviour is still among the modelled interpretations. If it returns a timestamp or text — which
/// is what these calls mostly are, `TIMESTAMP_TRUNC` and friends — the integer reading asserts laws
/// that do not hold of the real function, and the prover is reasoning about a program that does not
/// exist. `INTEGER` is only faithful in the first case; `VARBINARY` is faithful in both.
///
/// This was measured rather than assumed. Over a corpus of lowered cases, switching the default from
/// one to the other moved **zero verdicts** — the same pairs proved, case by case and not merely in
/// total. The weaker assumption is free here, so it is taken.
const UNDECLARED_RET: &str = "VARBINARY";

/// The two names a call answers to: its qualified spelling and its bare final component.
///
/// The distinction is load-bearing, because the name does two unrelated jobs.
///
/// As the **IR operator** it is the *identity* of an uninterpreted symbol, and must stay qualified:
/// collapsing `sales.total` and `hr.total` into one `TOTAL` would let the prover assume two
/// different functions are the same function, which is a false-proof channel.
///
/// As a **declaration key** it names a function, not a call site. `declare scalar function f(...)`
/// describes `f` however it is spelled at the call, so the lookup has to reach it from a qualified
/// call too. The preprocessor makes this concrete: sqlglot parses `pg_catalog.like_escape(a, b)`
/// into an `Anonymous` whose `.name` is the bare `like_escape` — which is what it writes into the
/// `declare` line — but renders the call back *qualified*. Keying only on the qualified spelling
/// throws that declaration away.
fn fn_names(f: &Function) -> (String, String) {
    let full = obj_name(&f.name).to_uppercase();
    (full.clone(), bare_name(&full).to_string())
}

/// The final component of a possibly-qualified name: `PG_CATALOG.LIKE_ESCAPE` -> `LIKE_ESCAPE`.
fn bare_name(full: &str) -> &str {
    full.rsplit('.').next().unwrap_or(full)
}

/// The type an [`OPAQUE_AGGS`] entry returns, if `name` is one.
fn opaque_agg_ret(name: &str) -> Option<&'static str> {
    OPAQUE_AGGS.iter().find(|(n, _)| *n == name).map(|(_, t)| *t)
}

/// Whether `name` is an aggregate this frontend knows about without being told: prover-native
/// ([`BUILTIN_AGGS`]) or uninterpreted ([`OPAQUE_AGGS`]).
fn is_known_agg(name: &str) -> bool {
    BUILTIN_AGGS.contains(&name) || opaque_agg_ret(name).is_some()
}

/// The declaration for a call: exact qualified name first, then the bare component.
///
/// Order matters. An exact match is a statement about *this* function and always wins; the bare
/// fallback is a weaker inference, used only when nothing was declared under the qualified name.
fn fn_decl<'a>(fns: &'a Fns, full: &str, bare: &str) -> Option<&'a FnDecl> {
    fns.get(full).or_else(|| fns.get(bare))
}

/// The declared return type of a function, or the default when it wasn't declared.
fn fn_ret(fns: &Fns, full: &str, bare: &str) -> String {
    fn_decl(fns, full, bare).map(|d| d.ret.clone()).unwrap_or_else(|| UNDECLARED_RET.to_string())
}

/// SOUNDNESS GUARD: a qualified call whose bare name is one of the aggregates we know about cannot
/// be classified, so it is refused rather than guessed at.
///
/// Neither reading is safe. Treating `pg_catalog.sum(x)` as the builtin asserts real summation
/// semantics for a symbol that, under some other schema, may be an unrelated user function.
/// Treating it as an ordinary scalar is worse: an aggregate on the scalar path becomes a per-row
/// function and turns one output row into one row per input row. Refusing costs completeness on a
/// spelling nobody writes by accident.
fn reject_qualified_builtin_agg(full: &str, bare: &str) -> Result<()> {
    if full != bare && is_known_agg(bare) {
        return Err(unsupported(format!("qualified builtin aggregate {full} (cannot classify)")));
    }
    Ok(())
}

/// SOUNDNESS GUARD: see [`NONDETERMINISTIC`]. Matched on the *bare* name, because `pg_catalog.random`
/// is still `random` and widening a refusal can only ever cost completeness.
fn reject_nondeterministic(full: &str, bare: &str) -> Result<()> {
    if NONDETERMINISTIC.contains(&bare) {
        return Err(unsupported(format!("non-deterministic function {full}")));
    }
    Ok(())
}

/// SOUNDNESS GUARD: see [`ORDER_SENSITIVE_AGGS`]. Bare-name matched for the same reason as
/// [`reject_nondeterministic`].
///
/// This fires wherever a call is lowered, not only in aggregate position, because the two positions
/// are wrong in different ways and neither is worth keeping: in aggregate position the bag would
/// determine the result, which is the unsound assumption, and in scalar position — where these
/// currently land, having matched no aggregate list — the cardinality is wrong outright.
fn reject_order_sensitive_agg(full: &str, bare: &str) -> Result<()> {
    if ORDER_SENSITIVE_AGGS.contains(&bare) {
        return Err(unsupported(format!("order-sensitive aggregate {full}")));
    }
    Ok(())
}

/// Lower a top-level query to a `Relation` Value.
pub fn lower_query(cat: &Catalog, fns: &Fns, q: &Query) -> Result<Value> {
    Ok(lower_query_ctx(cat, fns, q, &[])?.0)
}

/// Lower a query in an enclosing context (`outer` = visible enclosing bindings, empty at top level),
/// returning the relation and its output columns. The output columns are needed when the query is a
/// derived table or subquery so the enclosing query can resolve its columns.
fn lower_query_ctx(cat: &Catalog, fns: &Fns, q: &Query, outer: &[Binding]) -> Result<(Value, OutCols)> {
    // Every query node passes through here, so this is the one place a lock clause can be caught.
    // Identical ones were already dropped by `normalize::strip_identical_locks`; any left differ
    // between the sides, and the prover has no concurrency to tell them apart.
    if !q.locks.is_empty() {
        return Err(unsupported(
            "row-locking clause (FOR UPDATE / FOR SHARE) not identical on both sides",
        ));
    }
    let ord = OrderCtx::Known(q.order_by.as_ref());
    let (rel, out_cols, sortable) = lower_setexpr_ctx(cat, fns, q.body.as_ref(), outer, ord)?;
    let rel = apply_pagination(cat, fns, q, rel, &out_cols, sortable.as_ref())?;
    Ok((rel, out_cols))
}

/// Wrap a lowered body in the prover's `Sort` when the query takes a row slice.
///
/// `ORDER BY` on its own does not need a node: without a slice nothing downstream can observe the
/// order, and bag semantics make it immaterial, so it is dropped exactly as before. A slice is what
/// makes the order observable, and then the whole clause has to be carried.
///
/// # What `Sort` means to the prover, and why an opaque node still proves things
///
/// `Sort` evaluates to a chain of *uninterpreted* `HOp` applications (`relation.rs:436`) — one
/// `sort` per collation entry, bottoming out in `limit(count, offset, src)` or `offset(n, src)`.
/// Three cases are interpreted rather than opaque (`partial.rs:124`): `limit(0, _)` is the empty
/// bag, and `limit(1, ·)` over a degenerate source and `offset(0, ·)` are the identity.
///
/// Opaque does not mean inert. The memo key that mints the `HOp` symbol holds the
/// *SMT-canonicalized* source relation, so two sides whose bodies are equal modulo the solver —
/// commuted `AND`s, `a > 0` against `a >= 1`, commuted joins — land on the same symbol and the
/// wrapper is transparent to them. What congruence cannot see through is a difference in the
/// pagination *itself*; those pairs simply go unproved, which is the conservative direction.
///
/// # The collation index is a tuple position, not a level
///
/// Column references elsewhere in this module are absolute de-Bruijn levels (see [`crate::scope`]).
/// The collation index is not one: it is a 0-based position in the *source relation's output tuple*,
/// which here is `out_cols`. Confirmed against the prover's own fixtures — in
/// `tests/calcite/testSortProjectTranspose1.json` a `Sort` over a two-column `Project` carries
/// `collation: [[1, "INTEGER", "ASCENDING"]]`.
///
/// # Order is not canonicalized, deliberately
///
/// Evaluation pops the collation from the *end*, so entry order fixes the nesting of `sort` HOps and
/// therefore the symbol. Sorting or deduplicating the collation to make more pairs match would make
/// `ORDER BY a, b` congruent to `ORDER BY b, a`, which is a false-proof channel. The clause order is
/// preserved verbatim.
///
/// # An inherited assumption, stated because it is the one soft spot
///
/// Treating `sort` as a function of its source makes a slice deterministic, whereas real SQL leaves
/// the choice among tied rows — and all rows under a bare `LIMIT` with no `ORDER BY` — unspecified.
/// That is the prover's abstraction and the standard one, and it is the same assumption
/// [`crate::normalize::strip_identical_pagination`] already rests on; this function inherits it
/// rather than widening it.
fn apply_pagination(
    cat: &Catalog,
    fns: &Fns,
    q: &Query,
    rel: Value,
    out_cols: &OutCols,
    sortable: Option<&Scope>,
) -> Result<Value> {
    let Some((limit, offset)) = row_slice(cat, fns, q)? else { return Ok(rel) };
    let collation = match collation(q, out_cols)? {
        CollationPlan::Direct(c) => c,
        // At least one key is not an output column, so the `Sort` has nothing to point at until the
        // projection is widened. Only a plain select can be widened; see [`sort_sandwich`].
        CollationPlan::NeedsExtension => {
            let Some(scope) = sortable else {
                return Err(unsupported("ORDER BY key is not an output column"));
            };
            return sort_sandwich(cat, fns, scope, q, rel, out_cols, limit, offset);
        }
    };
    Ok(json!({
        "sort": { "collation": collation, "limit": limit, "offset": offset, "source": rel }
    }))
}

/// Calcite's `Project(trim) <- Sort <- Project(outputs ++ keys)`, for an `ORDER BY` over a value the
/// query does not output — `SELECT a FROM t ORDER BY b LIMIT 1`, or `ORDER BY lower(n)`.
///
/// The `Sort` collation is a position in its *source's* output tuple, so the only way to order by
/// something the projection dropped is to stop dropping it: lower the key in the select's own scope,
/// append it to the projection, point the collation at the new position, and trim it back off above
/// the `Sort` so the query's output is unchanged.
///
/// # Why this needs the select's scope, and why only a plain select gets one
///
/// The key is an arbitrary expression over the FROM bindings, which only [`lower_select_ctx`] has.
/// It passes its [`Scope`] up for exactly this, and only when its result is a projection directly
/// over the FROM relation. A `GROUP BY`, a `DISTINCT`, or a `DISTINCT ON` puts a `Group` in between,
/// and a FROM-scope expression cannot be addressed through one — appending it there would silently
/// order by whatever column happens to sit at that index. Those keep the refusal, as do set
/// operations and `VALUES`, which have no single FROM scope to resolve against.
///
/// Ambiguity is still refused rather than resolved: see [`order_key_value`].
#[allow(clippy::too_many_arguments)]
fn sort_sandwich(
    cat: &Catalog,
    fns: &Fns,
    scope: &Scope,
    q: &Query,
    rel: Value,
    out_cols: &OutCols,
    limit: Option<Value>,
    offset: Option<Value>,
) -> Result<Value> {
    use sqlparser::ast::OrderByKind;

    let Some(order_by) = &q.order_by else {
        // `CollationPlan::NeedsExtension` is only reachable with keys to resolve.
        return Err(unsupported("ORDER BY key is not an output column"));
    };
    let OrderByKind::Expressions(keys) = &order_by.kind else {
        return Err(unsupported("ORDER BY ALL"));
    };

    let base = scope.base;
    let (mut targets, source) = split_projection(&rel, out_cols, base);
    let mut collation = Vec::with_capacity(keys.len());
    for key in keys {
        if key.with_fill.is_some() {
            return Err(unsupported("ORDER BY ... WITH FILL"));
        }
        let v = order_key_value(cat, scope, fns, &key.expr, &targets, out_cols)?;
        let ty = ty_of(&v);
        let idx = push_unique(&mut targets, v);
        collation.push(json!([idx, ty, ord_string(&key.options)?]));
    }

    let extended = json!({ "project": { "target": targets, "source": source } });
    let sorted = json!({
        "sort": { "collation": collation, "limit": limit, "offset": offset, "source": extended }
    });
    // Trim the appended keys back off, so the query's output columns are the ones it declared.
    let trim: Vec<Value> = out_cols
        .iter()
        .enumerate()
        .map(|(i, (_, t))| json!({ "column": base + i, "type": t }))
        .collect();
    Ok(json!({ "project": { "target": trim, "source": sorted } }))
}

/// The `(limit, offset)` counts a query slices by, or `None` if it takes no slice.
///
/// `OFFSET` alone is a slice: it drops rows, so the order it drops them in is observable. `LIMIT`
/// alone is too, with no offset.
fn row_slice(
    cat: &Catalog,
    fns: &Fns,
    q: &Query,
) -> Result<Option<(Option<Value>, Option<Value>)>> {
    use sqlparser::ast::LimitClause;

    let (mut limit, mut offset) = (None, None);
    match &q.limit_clause {
        None => {}
        Some(LimitClause::LimitOffset { limit: l, offset: o, limit_by }) => {
            // ClickHouse `LIMIT n BY expr` keeps n rows *per group* — a different operator.
            if !limit_by.is_empty() {
                return Err(unsupported("LIMIT ... BY"));
            }
            limit = l.as_ref().map(|l| count(cat, fns, l)).transpose()?;
            offset = o.as_ref().map(|o| count(cat, fns, &o.value)).transpose()?;
        }
        // MySQL `LIMIT offset, limit`.
        Some(LimitClause::OffsetCommaLimit { offset: o, limit: l }) => {
            limit = Some(count(cat, fns, l)?);
            offset = Some(count(cat, fns, o)?);
        }
    }

    if let Some(f) = &q.fetch {
        // `WITH TIES` returns a variable number of rows, so the count is not the row count.
        if f.with_ties {
            return Err(unsupported("FETCH ... WITH TIES"));
        }
        if f.percent {
            return Err(unsupported("FETCH ... PERCENT"));
        }
        // `OFFSET n ROWS FETCH NEXT m ROWS ONLY` splits across both fields, so a FETCH alongside an
        // OFFSET-only LIMIT clause is standard. A FETCH alongside an actual LIMIT is contradictory.
        if limit.is_some() {
            return Err(unsupported("both LIMIT and FETCH"));
        }
        // Bare `FETCH FIRST ROW ONLY` means one row.
        limit = Some(match &f.quantity {
            Some(e) => count(cat, fns, e)?,
            None => json!({ "operator": "1", "operand": [], "type": "INTEGER" }),
        });
    }

    Ok((limit.is_some() || offset.is_some()).then_some((limit, offset)))
}

/// A `LIMIT`/`OFFSET` count, lowered as an ordinary expression against an empty scope.
///
/// The prover evaluates the count with `env.eval` like any other expression (`relation.rs:445`), so
/// it need not be a literal — and it must not be restricted to one, because the overwhelmingly
/// common shape in real query logs is `LIMIT $3`, which [`crate::casts::substitute_params`] has
/// already rewritten to the nullary constant `qp3(0)` by the time lowering runs.
///
/// The scope is empty on purpose. A count cannot reference a column, so there is nothing to resolve
/// against; passing the body's scope would silently accept `LIMIT a` as a column reference.
///
/// Only the interpreted cases care what the count *is*: `partial.rs:124` compares the evaluated
/// argument structurally against literal `0` and `1`, so `qp3(0)` matches neither and the `limit`
/// `HOp` stays opaque. Two sides sharing a parameter share its symbol, so `LIMIT $3` proves against
/// `LIMIT $3`; `LIMIT $3` against `LIMIT $4` is two different symbols and simply goes unproved.
/// The count must come out `INTEGER`. `infer`'s pass 7b normally sees to that, but a parameter with
/// competing `Conf::Cast` evidence can still land elsewhere, and a count on any other sort is not a
/// weaker proof — it is a `SortDiffers` panic inside z3 the moment congruence asserts the two sides'
/// counts equal (the prover's absent-offset default is an `Int` literal, so even one side suffices).
/// Refusing is the conservative direction and keeps a frontend gap from presenting as a prover crash.
fn count(cat: &Catalog, fns: &Fns, e: &Expr) -> Result<Value> {
    let v = lower_expr(cat, &Scope::empty(), fns, e)?;
    match ty_of(&v).as_str() {
        "INTEGER" => Ok(v),
        other => Err(unsupported(format!("LIMIT/OFFSET count typed {other}, not INTEGER"))),
    }
}

/// The collation for a sliced query: one entry per `ORDER BY` key, in clause order.
///
/// Empty is legal and meaningful — `LIMIT 5` with no `ORDER BY` lowers to a bare `limit` HOp.
fn collation(q: &Query, out_cols: &OutCols) -> Result<CollationPlan> {
    use sqlparser::ast::OrderByKind;

    let Some(order_by) = &q.order_by else { return Ok(CollationPlan::Direct(Vec::new())) };
    let keys = match &order_by.kind {
        OrderByKind::Expressions(keys) => keys,
        // `ORDER BY ALL` orders by every output column; the expansion is not worth guessing.
        OrderByKind::All(_) => return Err(unsupported("ORDER BY ALL")),
    };

    let mut out = Vec::with_capacity(keys.len());
    for key in keys {
        if key.with_fill.is_some() {
            return Err(unsupported("ORDER BY ... WITH FILL"));
        }
        let Some(idx) = order_key_index(&key.expr, out_cols)? else {
            return Ok(CollationPlan::NeedsExtension);
        };
        out.push(json!([idx, out_cols[idx].1, ord_string(&key.options)?]));
    }
    Ok(CollationPlan::Direct(out))
}

/// Whether a query's `ORDER BY` can be pointed straight at its output columns.
enum CollationPlan {
    /// Every key is an output column; the `Sort` stacks directly on the body.
    Direct(Vec<Value>),
    /// A key is an expression, or a column the projection drops. [`sort_sandwich`] widens the
    /// projection so the collation has something to address.
    NeedsExtension,
}

/// Resolve one `ORDER BY` key to a position in the query's own output columns, or `None` if it is
/// not one of them.
///
/// SQL also lets the key be an expression (`ORDER BY lower(n)`) or name a column the projection
/// *drops* (`SELECT a FROM t ORDER BY b`). Neither has a position here, and both are handled a level
/// up by [`sort_sandwich`], which widens the projection until they do. `None` asks for that; `Err`
/// is reserved for keys that are wrong rather than merely absent.
///
/// A wildcard needs no special case: `expand_projection` has already resolved `*` into named
/// columns by the time `out_cols` exists, so `SELECT * FROM t ORDER BY b` resolves like any other.
fn order_key_index(e: &Expr, out_cols: &OutCols) -> Result<Option<usize>> {
    // `ORDER BY 2` is a 1-based position in the select list.
    if let Expr::Value(v) = e {
        if let SqlValue::Number(n, _) = &v.value {
            let pos: usize =
                n.parse().map_err(|_| unsupported(format!("ORDER BY position {n}")))?;
            if pos >= 1 && pos <= out_cols.len() {
                return Ok(Some(pos - 1));
            }
            return Err(schema(format!("ORDER BY position {pos} out of range")));
        }
    }

    let name = match e {
        Expr::Identifier(id) => id.value.to_lowercase(),
        Expr::CompoundIdentifier(p) => p.last().unwrap().value.to_lowercase(),
        // An expression key: no output position, but `sort_sandwich` can give it one.
        _ => return Ok(None),
    };

    let mut found = out_cols.iter().enumerate().filter(|(_, (n, _))| *n == name);
    match (found.next(), found.next()) {
        (Some((i, _)), None) => Ok(Some(i)),
        // Two output columns of the same name: which one the key means is a resolution question we
        // decline rather than answer arbitrarily. Widening the projection would not help — the
        // ambiguity is in the name, not in what the query happens to output.
        (Some(_), Some(_)) => Err(schema(format!("ambiguous ORDER BY key {name}"))),
        // A source column the projection drops.
        _ => Ok(None),
    }
}

/// The direction tag for one collation entry.
///
/// Both defaults are resolved rather than passed through, so that `ORDER BY a` and
/// `ORDER BY a ASC NULLS LAST` — the same ordering, spelled two ways — produce the same tag and can
/// be proved equal.
///
/// Null placement is folded in because the collation tuple has nowhere else to put it: its three
/// fields are index, type, and this string. Leaving it out would make `NULLS FIRST` and `NULLS LAST`
/// mint the same symbol and prove equal, which is a false-proof channel. Postgres defaults are
/// `NULLS LAST` for ascending and `NULLS FIRST` for descending.
///
/// `USING <operator>` is refused: its direction is whatever the operator's btree class says, and
/// reading it as the ascending default would let `USING >` prove equal to `ASC`.
fn ord_string(o: &sqlparser::ast::OrderByOptions) -> Result<String> {
    use sqlparser::ast::OrderBySort;
    let desc = match &o.sort {
        None | Some(OrderBySort::Asc) => false,
        Some(OrderBySort::Desc) => true,
        Some(OrderBySort::Using(_)) => return Err(unsupported("ORDER BY ... USING <operator>")),
    };
    let dir = if desc { "DESCENDING" } else { "ASCENDING" };
    let nulls_first = o.nulls_first.unwrap_or(desc);
    Ok(format!("{dir} NULLS {}", if nulls_first { "FIRST" } else { "LAST" }))
}

/// The enclosing query's `ORDER BY`, threaded down for the one construct whose meaning depends on
/// it: `DISTINCT ON` (see [`distinct_on`]).
///
/// Nothing else in this module reads the clause — [`apply_pagination`] drops it when the query
/// takes no row slice, because without a slice bag semantics make the order unobservable. For
/// `DISTINCT ON` it is observable: the clause is what picks the surviving row. The clause hangs off
/// the enclosing [`Query`] while `DISTINCT ON` hangs off the [`Select`], so it has to be carried.
///
/// `Unknown` is deliberately distinct from `Known(None)`. A `SELECT` reached through a set
/// operation is governed by an `ORDER BY` that belongs to the set operation rather than to it, and
/// reading that as "no ORDER BY" would hand two differently-ordered `DISTINCT ON`s the same symbol
/// — a false-proof channel. `Unknown` refuses instead of guessing.
#[derive(Clone, Copy)]
enum OrderCtx<'a> {
    Known(Option<&'a OrderBy>),
    Unknown,
}

/// A select whose output columns are plain expressions over its own FROM scope, paired with that
/// scope — which is what lets an enclosing `Sort` order by a value the projection does not expose.
/// See [`sort_sandwich`] for why nothing else qualifies.
type SortScope = Option<Scope>;

/// Lower a set-expression body: a plain SELECT, a set operation, a parenthesized query, or VALUES.
fn lower_setexpr_ctx(
    cat: &Catalog,
    fns: &Fns,
    body: &SetExpr,
    outer: &[Binding],
    ord: OrderCtx,
) -> Result<(Value, OutCols, SortScope)> {
    match body {
        SetExpr::Select(s) => lower_select_ctx(cat, fns, s, outer, ord),
        // A parenthesized query carries its own ORDER BY; `ord` belongs to the enclosing one, and
        // its own `Sort` (if any) is already in place, so there is nothing left to widen.
        SetExpr::Query(q) => lower_query_ctx(cat, fns, q, outer).map(|(v, c)| (v, c, None)),
        SetExpr::SetOperation { op, set_quantifier, left, right } => {
            let (lv, lcols, _) = lower_setexpr_ctx(cat, fns, left, outer, OrderCtx::Unknown)?;
            let (rv, rcols, _) = lower_setexpr_ctx(cat, fns, right, outer, OrderCtx::Unknown)?;
            // Postgres resolves each output column to one type across both branches, promoting a
            // DATE branch against a TIMESTAMP one. The prover takes the columns as they stand, so a
            // branch pair that differs across a temporal boundary would put two units in one column;
            // with no conversion to insert inside a branch from here, it is refused.
            if let Some(((_, a), (_, b))) =
                lcols.iter().zip(&rcols).find(|((_, a), (_, b))| temporal_mismatch(a, b))
            {
                return Err(unsupported(format!("set operation over columns of type {a} and {b}")));
            }
            let all = matches!(set_quantifier, SetQuantifier::All | SetQuantifier::AllByName);
            let rel = match op {
                SetOperator::Union => {
                    let u = json!({ "union": [lv, rv] });
                    if all { u } else { json!({ "distinct": u }) }
                }
                // The prover's Intersect/Except are set-semantics (squash); ALL = bag => refuse.
                SetOperator::Intersect if all => return Err(unsupported("INTERSECT ALL (bag semantics)")),
                SetOperator::Intersect => json!({ "intersect": [lv, rv] }),
                // MINUS is a non-standard synonym for EXCEPT.
                SetOperator::Except | SetOperator::Minus if all => {
                    return Err(unsupported("EXCEPT/MINUS ALL (bag semantics)"))
                }
                SetOperator::Except | SetOperator::Minus => json!({ "except": [lv, rv] }),
            };
            Ok((rel, lcols, None))
        }
        SetExpr::Values(v) => lower_values(cat, fns, v).map(|(r, c)| (r, c, None)),
        other => Err(unsupported(format!("query body {other:?}"))),
    }
}

/// `VALUES (...), (...)` -> a prover `Values` relation.
fn lower_values(cat: &Catalog, fns: &Fns, v: &Values) -> Result<(Value, OutCols)> {
    if v.rows.is_empty() {
        return Err(unsupported("empty VALUES"));
    }
    let empty = Scope::empty();
    let content: Vec<Vec<Value>> = v
        .rows
        .iter()
        .map(|row| row.iter().map(|e| lower_expr(cat, &empty, fns, e)).collect::<Result<Vec<_>>>())
        .collect::<Result<Vec<_>>>()?;
    let schema_tys: Vec<String> = content[0].iter().map(ty_of).collect();
    // Same reason as the set operations: the column type is the first row's, and a later row of
    // another temporal type would hold a value in another unit.
    for row in &content[1..] {
        if let Some((a, b)) =
            schema_tys.iter().zip(row.iter().map(ty_of)).find(|(a, b)| temporal_mismatch(a, b))
        {
            return Err(unsupported(format!("VALUES column of type {a} holding a {b}")));
        }
    }
    let out_cols: OutCols =
        schema_tys.iter().enumerate().map(|(i, t)| (format!("$col{i}"), t.clone())).collect();
    Ok((json!({ "values": { "schema": schema_tys, "content": content } }), out_cols))
}

/// Whether `e` is *closed*: no column reference and no nested query anywhere inside it.
///
/// The walk is the derived AST visitor rather than a hand-written match on purpose. Every caller
/// uses this as a soundness guard, so the failure mode that matters is missing a variant — and a
/// hand-written match silently misses every variant added by a parser upgrade.
fn is_closed(e: &Expr) -> bool {
    visit_expressions(e, |x| match x {
        Expr::Identifier(_)
        | Expr::CompoundIdentifier(_)
        | Expr::CompoundFieldAccess { .. }
        | Expr::Subquery(_)
        | Expr::Exists { .. }
        | Expr::InSubquery { .. }
        | Expr::AnyOp { .. }
        | Expr::AllOp { .. } => ControlFlow::Break(()),
        _ => ControlFlow::Continue(()),
    })
    .is_continue()
}

/// A `FROM`-less `SELECT e1, ..., en`: one row, no input relation — i.e. `VALUES (e1, ..., en)`,
/// which is how it is emitted.
///
/// The restriction to closed expressions is what makes that rewrite safe. The prover evaluates a
/// `Values` row's content one level *above* the row itself (`Env(.., lvl + scope.len())`), so a
/// correlated column or a nested subquery in one of these expressions would have to be numbered
/// differently here than everywhere else in the frontend. Rather than special-case the numbering,
/// only closed expressions are accepted and every other FROM-less SELECT is refused.
fn lower_fromless_select(cat: &Catalog, fns: &Fns, s: &Select) -> Result<(Value, OutCols)> {
    // Every remaining clause needs a source row to mean anything; none of them are degenerate
    // enough to just drop, so a FROM-less SELECT carrying one is refused.
    if s.selection.is_some() || s.having.is_some() || !group_by_empty(s) || s.distinct.is_some() {
        return Err(unsupported("FROM-less SELECT with WHERE/GROUP BY/HAVING/DISTINCT"));
    }
    let empty = Scope::empty();
    let mut content: Vec<Value> = Vec::new();
    let mut out_cols: OutCols = Vec::new();
    for (idx, item) in s.projection.iter().enumerate() {
        let (e, name) = match item {
            SelectItem::UnnamedExpr(e) => (e, expr_name(e, idx)),
            SelectItem::ExprWithAlias { expr, alias } => (expr, alias.value.to_lowercase()),
            // A wildcard needs a FROM to expand against, so this is not valid SQL to begin with.
            other => return Err(unsupported(format!("FROM-less SELECT projection {other:?}"))),
        };
        if !is_closed(e) {
            return Err(unsupported("FROM-less SELECT over a column or subquery"));
        }
        // An aggregate here has no rows to fold over; `contains_agg` keeps it out of `Values`,
        // where it would otherwise be emitted as if it were a scalar.
        if contains_agg(fns, e) {
            return Err(unsupported("aggregate in a FROM-less SELECT"));
        }
        let v = lower_expr(cat, &empty, fns, e)?;
        out_cols.push((name, ty_of(&v)));
        content.push(v);
    }
    if content.is_empty() {
        return Err(unsupported("FROM-less SELECT with no projection"));
    }
    let schema: Vec<String> = content.iter().map(ty_of).collect();
    Ok((json!({ "values": { "schema": schema, "content": [content] } }), out_cols))
}

fn lower_select_ctx(
    cat: &Catalog,
    fns: &Fns,
    s: &Select,
    outer: &[Binding],
    ord: OrderCtx,
) -> Result<(Value, OutCols, SortScope)> {
    // SOUNDNESS GUARDS: refuse result-changing clauses we don't faithfully lower.
    if s.top.is_some() {
        return Err(unsupported("TOP"));
    }
    if !s.sort_by.is_empty() || !s.cluster_by.is_empty() || !s.distribute_by.is_empty() {
        return Err(unsupported("SORT/CLUSTER/DISTRIBUTE BY"));
    }
    if s.qualify.is_some() {
        return Err(unsupported("QUALIFY"));
    }
    if s.from.is_empty() {
        let (rel, cols) = lower_fromless_select(cat, fns, s)?;
        return Ok((rel, cols, None));
    }

    let (scope, mut rel) = build_from_clause(cat, fns, &s.from, outer)?;

    if let Some(w) = &s.selection {
        let cond = lower_bool(cat, &scope, fns, w)?;
        rel = json!({ "filter": { "condition": cond, "source": rel } });
    }

    let has_agg = s.projection.iter().any(|it| item_expr(it).is_some_and(|e| contains_agg(fns, e)))
        || s.having.as_ref().is_some_and(|h| contains_agg(fns, h));
    let aggregated = has_agg || !group_by_empty(s) || s.having.is_some();
    let (result, out_cols) = if aggregated {
        lower_aggregate(cat, &scope, fns, rel, s)?
    } else if is_pure_wildcard(s) && !scope.hides_columns() {
        // `SELECT *` over the FROM relation *is* that relation — but only while every one of its
        // columns is visible. A system column is not, so taking the shortcut there would hand the
        // caller a relation one column wider than the shape it was just told the query has.
        (rel, scope.out_cols())
    } else {
        let proj = expand_projection(cat, &scope, fns, s)?;
        let targets: Vec<Value> = proj.iter().map(|(_, v)| v.clone()).collect();
        let cols: OutCols = proj.iter().map(|(n, v)| (n.clone(), ty_of(v))).collect();
        (json!({ "project": { "target": targets, "source": rel } }), cols)
    };

    let (rel, cols) = apply_distinct(cat, fns, &scope, s, result, out_cols, ord, aggregated)?;
    // Only a projection sitting directly on the FROM relation can be widened with an ORDER BY key;
    // a `Group` in between makes the FROM scope unaddressable from above. See [`sort_sandwich`].
    let plain = !aggregated && matches!(s.distinct, None | Some(Distinct::All));
    Ok((rel, cols, plain.then_some(scope)))
}

/// `SELECT DISTINCT` -> Group keyed on all output columns (matches Calcite's canonical form, where
/// `DISTINCT` and a no-aggregate `GROUP BY` become the same Aggregate node). `DISTINCT ON` goes to
/// [`distinct_on`].
///
/// The keys index the projection underneath, which is a binder of our own making — hence
/// [`Scope::base`], not a plain `0..n`.
#[allow(clippy::too_many_arguments)]
fn apply_distinct(
    cat: &Catalog,
    fns: &Fns,
    scope: &Scope,
    s: &Select,
    result: Value,
    out_cols: OutCols,
    ord: OrderCtx,
    aggregated: bool,
) -> Result<(Value, OutCols)> {
    let base = scope.base;
    match &s.distinct {
        // `SELECT ALL` keeps duplicates -> same as no DISTINCT.
        None | Some(Distinct::All) => Ok((result, out_cols)),
        Some(Distinct::Distinct) => {
            let keys: Vec<Value> = out_cols
                .iter()
                .enumerate()
                .map(|(i, (_, t))| json!({ "column": base + i, "type": t }))
                .collect();
            Ok((json!({ "group": { "keys": keys, "function": [], "source": result } }), out_cols))
        }
        Some(Distinct::On(keys)) => {
            distinct_on(cat, fns, scope, keys, result, out_cols, ord, aggregated)
        }
    }
}

/// `SELECT DISTINCT ON (k…) c₁…cₙ FROM src ORDER BY …` -> a `Group` on `k…` whose columns are
/// *uninterpreted* aggregates, trimmed back to `c₁…cₙ`.
///
/// # Why it cannot be given exact semantics
///
/// `DISTINCT ON` keeps one row per distinct key — *the first one under the query's `ORDER BY`*. It is
/// order-sensitive, and the prover's relational algebra is bag-semantic, so there is no faithful
/// encoding. That is the bind `Sort` is already in, and the answer is the same one
/// [`apply_pagination`] uses: emit an opaque operator and let the prover's memo key decide when two
/// of them are the same.
///
/// # The encoding, and why it needs no new prover node
///
/// The prover's `Relation` enum has no generic opaque-relation node, but it does have an opaque
/// *aggregate*: `relation.rs:417` falls through to `HOp(op, [], Lambda(inner_scope, body), ty)` for
/// any aggregate op it does not recognise, where `body` sums over the tuples of the group. So an
/// `AggCall` with an invented name already **is** an uninterpreted higher-order term over the group's
/// multiset of rows — exactly the thing "pick one row of this group" needs.
///
/// The shape mirrors [`lower_aggregate`], as a sandwich:
///
/// ```text
/// Project(trim) <- Group{ keys, one AggCall per output column } <- Project(outputs ++ order keys)
/// ```
///
/// The bottom projection is the one non-obvious part and it is load-bearing; see *obligation 1*. The
/// top one trims because the prover's `Group` yields `keys ++ columns` while `DISTINCT ON` yields
/// only the columns.
///
/// Three properties of that fallthrough are easy to lose:
///
/// - **`ignoreNulls` must be `false`.** It defaults to `true` in the prover, which wraps the body in
///   `[not null]` predicates on the inner variables — that would silently drop every candidate row
///   with a NULL in any column, and `DISTINCT ON` drops nothing.
/// - **`args` must have length ≥ 2**, or the single-argument branch just above the fallthrough emits
///   an *interpreted* `Aggr` instead. Passing the whole extended tuple all but guarantees it, and
///   [`distinct_on_args`] pads the one degenerate case.
/// - **Do not reuse `Sort`.** A `Sort` with no row slice bottoms out in `HOp("offset", [0], src)`,
///   and `offset(0, ·)` is *interpreted as the identity* in `partial.rs` — so a `Sort`-based encoding
///   would silently equate `DISTINCT ON (k) …` with the plain `SELECT`.
///
/// `Group` with a non-empty key list takes the `squash(sum(…))` branch at `relation.rs:427`, so the
/// empty-keys scalar-aggregation guard is not in play; `DISTINCT ON` always has at least one key.
///
/// # Soundness obligation 1: the ordering values must be *in* the term
///
/// Two sides collapse onto one term exactly when the op name, the arguments, the keys and the source
/// all agree — the last three structurally, and modulo everything the solver already knows about the
/// source. Everything that distinguishes one `DISTINCT ON` from another therefore has to be visible
/// in one of those four places, or two *different* operators compare equal.
///
/// The trap is that the `ORDER BY` is visible in none of them. A query taking no row slice never
/// builds a `Sort`, so the clause reaches this function and nowhere else. Naming it in the op name is
/// **not** enough, because identical clause text can resolve to different values:
///
/// ```text
/// A: SELECT DISTINCT ON (k) k, v FROM ev                            ORDER BY k, t DESC
/// B: SELECT DISTINCT ON (k) k, v FROM (SELECT k, v, -t AS t FROM ev) ORDER BY k, t DESC
/// ```
///
/// Both print `ORDER BY k, t DESC`, both project `(k, v)`, and both sit on a projection that has
/// already dropped `t` — so with only the outputs as arguments the two terms are identical, while A
/// takes the largest `t` of each group and B the smallest. That pair proves equal, wrongly.
///
/// The fix is the bottom projection: the `ORDER BY` key expressions are lowered, appended to it, and
/// passed as arguments alongside the outputs. The lambda then binds the ordering values, `-t` and `t`
/// are structurally different sources, and the two sides no longer unify. The op name is left holding
/// only what is genuinely not a value — for each key, *which argument* it is and in *which direction*
/// it sorts (see [`order_digest`]). That digest is built from resolved positions and canonicalized
/// directions rather than from clause text, so `ORDER BY t` and `ORDER BY ev.t ASC NULLS LAST` are the
/// same operator, as they should be.
///
/// With that in place the argument closes: if two `DISTINCT ON`s land on one term then their extended
/// projections are structurally equal — same outputs, same ordering values, in the same positions —
/// their key lists are equal, their sources are equal to the solver, and the digest pins the same
/// argument positions to the same directions. They are the same operator.
///
/// # Soundness obligation 2: per-column aggregates over-approximate, safely
///
/// Modelling each output column as its own uninterpreted aggregate lets a model draw column 1 from
/// one row of the group and column 2 from another — something the real operator never does. That
/// admits *more* behaviours than SQL, so it can only ever cause a failure to prove, never a false
/// proof. A pair proved under it is equal for the real operator too.
///
/// # `DISTINCT ON` with no `ORDER BY`
///
/// Postgres then picks an arbitrary row. Two such queries land on the same symbol and can be proved
/// equal, which reads the nondeterminism as "the same implementation makes the same choice on equal
/// inputs" — precisely the assumption `HOp("limit", …)` already makes for `LIMIT` without `ORDER BY`
/// (see [`apply_pagination`]). This inherits that convention rather than widening it.
#[allow(clippy::too_many_arguments)]
fn distinct_on(
    cat: &Catalog,
    fns: &Fns,
    scope: &Scope,
    keys: &[Expr],
    result: Value,
    out_cols: OutCols,
    ord: OrderCtx,
    aggregated: bool,
) -> Result<(Value, OutCols)> {
    // `DISTINCT ON` over a grouped query resolves its keys against the *post-aggregation* output,
    // not the FROM scope, so the key expressions lowered here would address the wrong tuple. Rare
    // enough not to be worth a second resolution path.
    if aggregated {
        return Err(unsupported("DISTINCT ON over an aggregate query"));
    }
    if keys.is_empty() {
        return Err(unsupported("DISTINCT ON with no keys"));
    }
    // Not the same as "no ORDER BY": see [`OrderCtx`]. Guessing here is a false-proof channel.
    let OrderCtx::Known(order_by) = ord else {
        return Err(unsupported("DISTINCT ON under a set operation"));
    };

    let base = scope.base;
    let (mut targets, source) = split_projection(&result, &out_cols, base);

    // Extend the projection with anything the operator depends on that the outputs do not already
    // carry: the DISTINCT ON keys, then the ORDER BY keys. `push_unique` reuses an existing column
    // when the lowered expression is already there, so the common case appends nothing.
    let mut gkeys = Vec::with_capacity(keys.len());
    for k in keys {
        let v = lower_expr(cat, scope, fns, k)?;
        let ty = ty_of(&v);
        let idx = push_unique(&mut targets, v);
        gkeys.push(json!({ "column": base + idx, "type": ty }));
    }
    let digest = match order_by {
        Some(o) => order_digest(cat, scope, fns, o, &mut targets, &out_cols)?,
        None => String::new(),
    };

    let args = distinct_on_args(&targets, base);
    let funcs: Vec<Value> = out_cols
        .iter()
        .enumerate()
        .map(|(i, (_, t))| {
            json!({
                "operator": format!("DISTINCT_ON#{i}<{digest}>"),
                "operand": args,
                "type": t,
                "distinct": false,
                // See the soundness notes: the default is `true`, which would drop candidate rows.
                "ignoreNulls": false,
            })
        })
        .collect();

    let m = gkeys.len();
    let inner = json!({ "project": { "target": targets, "source": source } });
    let group = json!({ "group": { "keys": gkeys, "function": funcs, "source": inner } });
    // `Group` yields `keys ++ columns`; `DISTINCT ON` yields only the columns.
    let trim: Vec<Value> = out_cols
        .iter()
        .enumerate()
        .map(|(i, (_, t))| json!({ "column": base + m + i, "type": t }))
        .collect();
    Ok((json!({ "project": { "target": trim, "source": group } }), out_cols))
}

/// The argument list every `DISTINCT ON` aggregate is a lambda over: the whole extended tuple, so the
/// term ranges over exactly the candidate rows *and* the values that choose between them.
///
/// A one-column tuple is padded with a repeat of itself, because a single-argument `AggCall` whose
/// argument type equals its result type takes the *interpreted* `Aggr` branch at `relation.rs:414`
/// instead of the opaque fallthrough. The duplicate is inert — it adds a second bound variable
/// constrained to the same value — and it keeps the operator uninterpreted. Only a one-column
/// `DISTINCT ON` on its own output column with no `ORDER BY` gets that far.
fn distinct_on_args(targets: &[Value], base: usize) -> Vec<Value> {
    let mut args: Vec<Value> = targets
        .iter()
        .enumerate()
        .map(|(i, v)| json!({ "column": base + i, "type": ty_of(v) }))
        .collect();
    if args.len() == 1 {
        args.push(args[0].clone());
    }
    args
}

/// A relation's output expressions and the relation they are computed over.
///
/// A `Project` splits into exactly that. The only other relation reaching here is a bare `SELECT *`
/// body, which exposes its source columns positionally — the identity list over itself. (A `Group`
/// cannot reach here: [`distinct_on`] refuses aggregated queries first.)
fn split_projection(result: &Value, out_cols: &OutCols, base: usize) -> (Vec<Value>, Value) {
    let proj = result.get("project");
    let target = proj.and_then(|p| p.get("target")).and_then(|t| t.as_array());
    let source = proj.and_then(|p| p.get("source"));
    if let (Some(t), Some(s)) = (target, source) {
        return (t.clone(), s.clone());
    }
    let identity = out_cols
        .iter()
        .enumerate()
        .map(|(i, (_, t))| json!({ "column": base + i, "type": t }))
        .collect();
    (identity, result.clone())
}

/// Position of `v` in `targets`, appending it first if it is not already there.
fn push_unique(targets: &mut Vec<Value>, v: Value) -> usize {
    match targets.iter().position(|t| *t == v) {
        Some(i) => i,
        None => {
            targets.push(v);
            targets.len() - 1
        }
    }
}

/// A canonical string for an `ORDER BY` clause, naming the *argument positions* it orders by rather
/// than the text it was written as. Extends `targets` with any key it has to add.
///
/// One `pos:direction;` entry per key, in clause order. Positions are indices into the extended
/// projection, which both sides of a pair must share structurally for their terms to unify at all, so
/// they mean the same thing on both sides. Directions go through [`ord_string`], which resolves the
/// `ASC`/`NULLS` defaults, so `ORDER BY a` and `ORDER BY a ASC NULLS LAST` — one ordering spelled two
/// ways — digest identically and can be proved equal.
///
/// Clause forms whose effect is not captured by "value, direction, null placement" are refused rather
/// than digested to something that ignores them.
fn order_digest(
    cat: &Catalog,
    scope: &Scope,
    fns: &Fns,
    ord: &OrderBy,
    targets: &mut Vec<Value>,
    out_cols: &OutCols,
) -> Result<String> {
    use sqlparser::ast::OrderByKind;

    if ord.interpolate.is_some() {
        return Err(unsupported("ORDER BY ... INTERPOLATE"));
    }
    let keys = match &ord.kind {
        OrderByKind::Expressions(keys) => keys,
        OrderByKind::All(_) => return Err(unsupported("ORDER BY ALL")),
    };
    let mut out = String::new();
    for key in keys {
        if key.with_fill.is_some() {
            return Err(unsupported("ORDER BY ... WITH FILL"));
        }
        let v = order_key_value(cat, scope, fns, &key.expr, targets, out_cols)?;
        let pos = push_unique(targets, v);
        out.push_str(&format!("{pos}:{};", ord_string(&key.options)?));
    }
    Ok(out)
}

/// The value one `ORDER BY` key orders by, lowered.
///
/// Postgres resolves an `ORDER BY` key against the select list before the FROM scope, and this
/// follows that order: an integer literal is a 1-based select-list position, a bare identifier naming
/// exactly one output column is that column, and anything else is an ordinary expression over the
/// input. Getting the precedence backwards would silently order by the wrong value in
/// `SELECT b AS a FROM t ORDER BY a`.
///
/// `targets` is read for the first two cases and is *not* extended here; the caller records the
/// position.
fn order_key_value(
    cat: &Catalog,
    scope: &Scope,
    fns: &Fns,
    e: &Expr,
    targets: &[Value],
    out_cols: &OutCols,
) -> Result<Value> {
    if let Expr::Value(v) = e {
        if let SqlValue::Number(n, _) = &v.value {
            let pos: usize = n.parse().map_err(|_| unsupported(format!("ORDER BY position {n}")))?;
            return match pos.checked_sub(1).and_then(|i| targets.get(i)) {
                Some(t) if pos <= out_cols.len() => Ok(t.clone()),
                _ => Err(schema(format!("ORDER BY position {pos} out of range"))),
            };
        }
    }
    if let Expr::Identifier(id) = e {
        let name = id.value.to_lowercase();
        let mut found = out_cols.iter().enumerate().filter(|(_, (n, _))| *n == name);
        match (found.next(), found.next()) {
            (Some((i, _)), None) => return Ok(targets[i].clone()),
            // Two output columns of the same name: which one the key means is a resolution question
            // we decline rather than answer arbitrarily.
            (Some(_), Some(_)) => return Err(schema(format!("ambiguous ORDER BY key {name}"))),
            _ => {}
        }
    }
    lower_expr(cat, scope, fns, e)
}

/// A join step in a FROM clause. With both `on` and `precomputed` `None` this is an unrestricted
/// join (a comma-separated item or a `CROSS JOIN`), lowered to a join on `TRUE`. `on` borrows the
/// condition from the FROM AST (lifetime `'a`); `precomputed` carries one we built ourselves, which
/// is how `USING` arrives — its equalities are resolved against the two sides of that one join
/// rather than the finished scope.
struct Step<'a> {
    on: Option<&'a Expr>,
    kind: &'static str,
    precomputed: Option<Value>,
}

/// Build the resolution scope and relation tree for a whole FROM clause (comma items become cross
/// joins; each item may carry joins; factors may be base tables or derived tables). `outer` are the
/// enclosing-query bindings; this query's own bindings are offset past them so de-Bruijn indices stay
/// absolute across nesting.
fn build_from_clause<'a>(
    cat: &Catalog,
    fns: &Fns,
    from: &'a [TableWithJoins],
    outer: &[Binding],
) -> Result<(Scope, Value)> {
    let base = Scope::outer_width(outer);
    let mut binds: Vec<Binding> = Vec::new();
    let mut leaves: Vec<Value> = Vec::new();
    let mut steps: Vec<Step<'a>> = Vec::new();
    let mut offset = base;
    let mut merged: Vec<String> = Vec::new();
    let mut merged_outer = false;

    for item in from {
        let (b, leaf) = factor_instance(cat, fns, &item.relation, offset, outer)?;
        offset += b.cols.len();
        binds.push(b);
        leaves.push(leaf);
        steps.push(Step { on: None, kind: "INNER", precomputed: None }); // first of item: cross-join unless it's the very first
        for j in &item.joins {
            let (b, leaf) = factor_instance(cat, fns, &j.relation, offset, outer)?;
            offset += b.cols.len();
            let (kind, cond) = join_op(&j.join_operator)?;
            // `USING` is resolved here, against the bindings as they stand: its names are looked up
            // on the left of this join and on the factor being added, not through the whole scope.
            let on = match cond {
                JoinCond::On(e) => Some(e),
                JoinCond::Always => None,
                JoinCond::Using(cols) => {
                    if kind != "INNER" {
                        merged_outer = true;
                    }
                    let c = using_condition(&binds, &b, &cols)?;
                    merged.extend(cols);
                    steps.push(Step { on: None, kind, precomputed: Some(c) });
                    binds.push(b);
                    leaves.push(leaf);
                    continue;
                }
            };
            binds.push(b);
            leaves.push(leaf);
            steps.push(Step { on, kind, precomputed: None });
        }
    }
    let inner_count = binds.len();
    binds.extend(outer.iter().cloned()); // outer appended for correlated resolution only
    let scope = Scope { binds, inner_count, base, merged, merged_outer };

    let mut rel: Option<Value> = None;
    for (i, step) in steps.iter().enumerate() {
        let leaf = leaves[i].clone();
        rel = Some(match rel {
            None => leaf,
            Some(left) => {
                let cond = match (&step.precomputed, step.on) {
                    (Some(c), _) => c.clone(),
                    // Only the bindings this join actually has in its row -- see [`Scope::prefix`].
                    (None, Some(on)) => lower_bool(cat, &scope.prefix(i + 1), fns, on)?,
                    (None, None) => json!({ "operator": "TRUE", "operand": [], "type": "BOOLEAN" }),
                };
                json!({ "join": { "condition": cond, "left": left, "right": leaf, "kind": step.kind } })
            }
        });
    }
    Ok((scope, rel.expect("non-empty FROM")))
}

/// A single FROM relation factor -> (binding with output columns, leaf relation Value).
fn factor_instance(cat: &Catalog, fns: &Fns, tf: &TableFactor, offset: usize, outer: &[Binding]) -> Result<(Binding, Value)> {
    match tf {
        TableFactor::Table {
            name,
            alias,
            args,
            version,
            with_ordinality,
            partitions,
            json_path,
            sample,
            with_hints: _,
            index_hints: _,
        } => {
            // SOUNDNESS GUARDS: each of these changes which rows the factor yields, and matching on
            // the fields explicitly (rather than `..`) is what keeps a parser upgrade from adding a
            // new one that we then silently drop.
            //   args           a table-valued function, not this catalog table
            //   version        time travel — a different snapshot of the table
            //   with_ordinality  adds a row-number column, so the shape differs
            //   partitions     restricts to some partitions, so rows are missing
            //   json_path      navigates into the value rather than scanning it
            //   sample         TABLESAMPLE, which is not even deterministic
            let modifier = if args.is_some() {
                Some("table-valued function arguments")
            } else if version.is_some() {
                Some("FOR SYSTEM_TIME / version qualifier")
            } else if *with_ordinality {
                Some("WITH ORDINALITY")
            } else if !partitions.is_empty() {
                Some("PARTITION (...)")
            } else if json_path.is_some() {
                Some("PartiQL JSON path")
            } else if sample.is_some() {
                Some("TABLESAMPLE")
            } else {
                None
            };
            if let Some(m) = modifier {
                return Err(unsupported(format!("table factor with {m}")));
            }
            let tn = obj_name(name);
            let idx = cat.find(&tn).ok_or_else(|| schema(format!("unknown table {tn}")))?;
            let alias = alias
                .as_ref()
                .map(|a| a.name.value.clone())
                .unwrap_or_else(|| tn.clone())
                .to_lowercase();
            Ok((
                Binding {
                    alias,
                    cols: cat.tables[idx].cols.clone(),
                    offset,
                    table: Some(idx),
                    n_declared: cat.tables[idx].n_declared,
                },
                json!({ "scan": idx }),
            ))
        }
        TableFactor::Derived { subquery, alias, lateral, sample } => {
            // TABLESAMPLE is not deterministic, so it cannot be modelled at all.
            if sample.is_some() {
                return Err(unsupported("derived table with TABLESAMPLE"));
            }
            // A derived table is lowered against the enclosing context only, never its FROM
            // siblings -- which is exactly right for the non-lateral form. A `LATERAL` one *may*
            // see its siblings, and while a reference to one would usually just fail to resolve
            // here, it would not if a sibling shared an alias with an enclosing binding: SQL
            // resolves that to the sibling and we would silently reach the outer one instead.
            if *lateral {
                return Err(unsupported("LATERAL derived table"));
            }
            let (rel, out_cols) = lower_query_ctx(cat, fns, subquery, outer)?;
            let a = alias.as_ref().ok_or_else(|| schema("derived table requires an alias"))?;
            let cols = if a.columns.is_empty() {
                out_cols
            } else {
                if a.columns.len() != out_cols.len() {
                    return Err(schema("derived table column-alias count mismatch"));
                }
                a.columns.iter().zip(out_cols).map(|(c, (_, t))| (c.name.value.to_lowercase(), t)).collect()
            };
            // No `table`: a derived table has no declared keys, so nothing it outputs can be shown
            // functionally dependent on a GROUP BY key.
            // A derived table's columns are its output, so all of them are visible.
            let n_declared = cols.len();
            Ok((
                Binding { alias: a.name.value.to_lowercase(), cols, offset, table: None, n_declared },
                rel,
            ))
        }
        other => Err(unsupported(format!("FROM factor {other:?}"))),
    }
}

/// How a join restricts its two sides.
enum JoinCond<'a> {
    On(&'a Expr),
    /// `CROSS JOIN` / `ON TRUE`: no restriction, just the product.
    Always,
    /// `USING (c, ...)`: an equality per named column, plus a column merge (see [`Scope::merged`]).
    Using(Vec<String>),
}

fn join_op(op: &JoinOperator) -> Result<(&'static str, JoinCond<'_>)> {
    use JoinOperator::*;
    let (kind, c) = match op {
        // `JOIN`/`INNER JOIN`, `LEFT [OUTER]`, `RIGHT [OUTER]`, `FULL OUTER`.
        Join(c) | Inner(c) => ("INNER", c),
        Left(c) | LeftOuter(c) => ("LEFT", c),
        Right(c) | RightOuter(c) => ("RIGHT", c),
        FullOuter(c) => ("FULL", c),
        // A cross join carries `JoinConstraint::None`, which the constraint match below turns
        // into the unrestricted product -- exactly what a cross join is.
        CrossJoin(c) => ("INNER", c),
        other => return Err(unsupported(format!("join operator {other:?}"))),
    };
    match c {
        JoinConstraint::On(e) => Ok((kind, JoinCond::On(e))),
        JoinConstraint::None => Ok((kind, JoinCond::Always)),
        JoinConstraint::Using(names) => {
            let cols = names
                .iter()
                .map(|n| obj_name(n).split('.').next_back().unwrap().to_lowercase())
                .collect();
            Ok((kind, JoinCond::Using(cols)))
        }
        other => Err(unsupported(format!("join constraint {other:?}"))),
    }
}

/// The `ON` equalities a `USING (c, ...)` stands for: `left.c = right.c` for each name, where
/// `left` is everything joined so far and `right` is the factor being joined in.
fn using_condition(left: &[Binding], right: &Binding, cols: &[String]) -> Result<Value> {
    let mut terms: Vec<Value> = Vec::new();
    for c in cols {
        // SQL requires the name to be present and unambiguous on each side.
        let find = |bs: &[Binding]| -> Option<(usize, String)> {
            bs.iter().find_map(|b| {
                b.cols.iter().position(|(n, _)| n == c).map(|i| (b.offset + i, b.cols[i].1.clone()))
            })
        };
        let (li, lt) = find(left).ok_or_else(|| schema(format!("USING column {c} not on the left")))?;
        let (ri, rt) =
            find(std::slice::from_ref(right)).ok_or_else(|| schema(format!("USING column {c} not on the right")))?;
        terms.push(make_cmp("=", json!({ "column": li, "type": lt }), json!({ "column": ri, "type": rt })));
    }
    Ok(match terms.len() {
        0 => json!({ "operator": "TRUE", "operand": [], "type": "BOOLEAN" }),
        1 => terms.pop().unwrap(),
        _ => json!({ "operator": "AND", "operand": terms, "type": "BOOLEAN" }),
    })
}

fn is_pure_wildcard(s: &Select) -> bool {
    s.projection.len() == 1 && matches!(s.projection[0], SelectItem::Wildcard(_))
}

/// Expand a (non-aggregate) projection to `(output name, lowered Value)` pairs, expanding `*` and
/// `t.*` to explicit column references.
fn expand_projection(cat: &Catalog, scope: &Scope, fns: &Fns, s: &Select) -> Result<Vec<(String, Value)>> {
    let mut out: Vec<(String, Value)> = Vec::new();
    for (idx, item) in s.projection.iter().enumerate() {
        match item {
            SelectItem::UnnamedExpr(e) => out.push((expr_name(e, idx), lower_expr(cat, scope, fns, e)?)),
            SelectItem::ExprWithAlias { expr, alias } => {
                out.push((alias.value.to_lowercase(), lower_expr(cat, scope, fns, expr)?))
            }
            SelectItem::Wildcard(_) => {
                // `USING` merges each named pair into one output column, so a bare `*` here has
                // fewer columns than the same query written with `ON`. We do not model the merge,
                // so expanding `*` would silently give the two forms the same shape. A qualified
                // `t.*` is unaffected (it names one side) and stays supported.
                if !scope.merged.is_empty() {
                    return Err(unsupported("bare * over a JOIN ... USING (merged columns)"));
                }
                // `n_declared`, not `cols.len()`: a Postgres system column is readable by name
                // but `SELECT *` does not return it. See `catalog::add_system_columns`.
                for b in scope.inner() {
                    for (i, (n, t)) in b.cols[..b.n_declared].iter().enumerate() {
                        out.push((n.clone(), json!({ "column": b.offset + i, "type": t })));
                    }
                }
            }
            SelectItem::ExprWithAliases { .. } => {
                return Err(unsupported("multi-alias projection (expr AS (a, b))"))
            }
            SelectItem::QualifiedWildcard(kind, _) => {
                let name = match kind {
                    SelectItemQualifiedWildcardKind::ObjectName(n) => n,
                    SelectItemQualifiedWildcardKind::Expr(_) => {
                        return Err(unsupported("expression.* wildcard"))
                    }
                };
                let q = obj_name(name).split('.').next_back().unwrap().to_lowercase();
                let mut found = false;
                for b in scope.inner() {
                    if b.alias == q {
                        found = true;
                        for (i, (n, t)) in b.cols[..b.n_declared].iter().enumerate() {
                            out.push((n.clone(), json!({ "column": b.offset + i, "type": t })));
                        }
                    }
                }
                if !found {
                    return Err(schema(format!("unknown qualifier {q}.*")));
                }
            }
        }
    }
    Ok(out)
}

fn expr_name(e: &Expr, idx: usize) -> String {
    match e {
        Expr::Identifier(id) => id.value.to_lowercase(),
        Expr::CompoundIdentifier(p) => p.last().unwrap().value.to_lowercase(),
        _ => format!("$col{idx}"),
    }
}

fn group_by_empty(s: &Select) -> bool {
    matches!(&s.group_by, GroupByExpr::Expressions(v, _) if v.is_empty())
}

fn group_by_exprs(s: &Select) -> Result<Vec<&Expr>> {
    match &s.group_by {
        // `GROUP BY (a, b, c)` is a row constructor, and grouping on the row is the same partition
        // as grouping on its members: grouping compares with NULLs equal, and row comparison is
        // field-wise, so two rows agree on the row value exactly when they agree on every member.
        // Flattening it is also what lets the projection reach the members — `SELECT a` matches the
        // key `a`, where against a single row-valued key it would look like a non-grouped column.
        GroupByExpr::Expressions(v, _) => Ok(v
            .iter()
            .flat_map(|e| match e {
                Expr::Tuple(items) => items.iter().collect::<Vec<_>>(),
                other => vec![other],
            })
            .collect()),
        GroupByExpr::All(_) => Err(unsupported("GROUP BY ALL")),
    }
}

fn item_expr(it: &SelectItem) -> Option<&Expr> {
    match it {
        SelectItem::UnnamedExpr(e) => Some(e),
        SelectItem::ExprWithAlias { expr, .. } => Some(expr),
        _ => None,
    }
}

struct Agg {
    op: String,
    args: Vec<Expr>,
    distinct: bool,
    /// The `FILTER (WHERE p)` predicate, evaluated per input row before the fold.
    filter: Option<Expr>,
}

/// Extract a function call's positional argument list, plus whether it carries DISTINCT or an
/// ORDER BY clause. Returns an error for a sole-subquery argument (which we don't lower).
fn fn_args(f: &Function) -> Result<(&[FunctionArg], bool, bool)> {
    match &f.args {
        FunctionArguments::List(l) => Ok((
            &l.args,
            matches!(l.duplicate_treatment, Some(DuplicateTreatment::Distinct)),
            l.clauses.iter().any(|c| matches!(c, FunctionArgumentClause::OrderBy(_))),
        )),
        FunctionArguments::None => Ok((&[], false, false)),
        FunctionArguments::Subquery(_) => Err(unsupported("function with a subquery argument")),
    }
}

/// Whether `e` is an aggregate function call: one of the builtins, or a name the input declared with
/// `declare aggregate function`. A windowed call (`OVER`) is not an aggregate for grouping purposes.
fn is_agg_call(fns: &Fns, e: &Expr) -> bool {
    let Expr::Function(f) = e else { return false };
    if f.over.is_some() {
        return false;
    }
    // The names we know on our own are matched on the qualified spelling only — `myschema.sum` is
    // not assumed to be `SUM` (see [`reject_qualified_builtin_agg`], which refuses that case
    // downstream). A *declared* aggregate is matched on either spelling: missing it here is the
    // dangerous direction, since the call would then take the scalar path and inflate the row count.
    let (full, bare) = fn_names(f);
    is_known_agg(&full) || fn_decl(fns, &full, &bare).is_some_and(|d| d.aggregate)
}

/// Whether `e` contains an aggregate call anywhere in *this* query's expression tree.
///
/// `SELECT COALESCE(SUM(a), 0) FROM t` is an aggregate query even though the projection item is a
/// `COALESCE`, so looking only at the top of each item would lower `SUM` as a per-row scalar and
/// silently produce one output row per input row. Recursion deliberately stops at subqueries (an
/// aggregate in there belongs to the subquery) and at windowed calls (not aggregates here).
///
/// This is a *completeness* aid, not the safety net: any aggregate this misses is still caught by
/// the refusal in [`lower_expr`]'s function arm, so a gap here costs coverage, never soundness.
fn contains_agg(fns: &Fns, e: &Expr) -> bool {
    if is_agg_call(fns, e) {
        return true;
    }
    let any = |es: &[Expr]| es.iter().any(|x| contains_agg(fns, x));
    match e {
        Expr::Nested(i) | Expr::UnaryOp { expr: i, .. } | Expr::Cast { expr: i, .. } => {
            contains_agg(fns, i)
        }
        Expr::IsNull(i) | Expr::IsNotNull(i) | Expr::IsTrue(i) | Expr::IsNotTrue(i) => {
            contains_agg(fns, i)
        }
        Expr::IsFalse(i) | Expr::IsNotFalse(i) | Expr::IsUnknown(i) | Expr::IsNotUnknown(i) => {
            contains_agg(fns, i)
        }
        Expr::BinaryOp { left, right, .. }
        | Expr::IsDistinctFrom(left, right)
        | Expr::IsNotDistinctFrom(left, right) => contains_agg(fns, left) || contains_agg(fns, right),
        Expr::Between { expr, low, high, .. } => {
            contains_agg(fns, expr) || contains_agg(fns, low) || contains_agg(fns, high)
        }
        Expr::Like { expr, pattern, .. }
        | Expr::ILike { expr, pattern, .. }
        | Expr::SimilarTo { expr, pattern, .. } => {
            contains_agg(fns, expr) || contains_agg(fns, pattern)
        }
        Expr::InList { expr, list, .. } => contains_agg(fns, expr) || any(list),
        Expr::Tuple(es) => any(es),
        Expr::Case { operand, conditions, else_result, .. } => {
            operand.as_deref().is_some_and(|o| contains_agg(fns, o))
                || conditions
                    .iter()
                    .any(|w| contains_agg(fns, &w.condition) || contains_agg(fns, &w.result))
                || else_result.as_deref().is_some_and(|x| contains_agg(fns, x))
        }
        // A windowed call's arguments are not this query's aggregates, and `OVER` is refused anyway.
        Expr::Function(f) if f.over.is_none() => match &f.args {
            FunctionArguments::List(l) => l.args.iter().any(|a| match a {
                FunctionArg::Unnamed(FunctionArgExpr::Expr(x)) => contains_agg(fns, x),
                _ => false,
            }),
            _ => false,
        },
        _ => false,
    }
}

/// Extract an aggregate (assumes [`is_agg_call`]); refuses unsupported modifiers.
fn agg_of(e: &Expr) -> Result<Agg> {
    let f = match e {
        Expr::Function(f) => f,
        _ => unreachable!("agg_of on non-function"),
    };
    let name = obj_name(&f.name).to_uppercase();
    // SOUNDNESS GUARD: see [`ORDER_SENSITIVE_AGGS`]. The two call-lowering sites already refuse these,
    // but neither is reached from here: an input that says `declare aggregate function array_agg(...)`
    // makes [`is_agg_call`] true and sends the call straight down the Group path. A declaration is a
    // statement about the return type, not permission to assume the bag determines the value.
    reject_order_sensitive_agg(&name, bare_name(&name))?;
    let (raw_args, distinct, has_order) = fn_args(f)?;
    if has_order || f.null_treatment.is_some() {
        return Err(unsupported(format!("aggregate modifier (ORDER BY/null-treatment) in {name}")));
    }
    // `FILTER (WHERE p)` is lowered by pushing the predicate into the argument as
    // `CASE WHEN p THEN arg END` (see [`AggCtx::add_agg`]), which is only faithful for aggregates
    // that skip NULL inputs. That is exactly the builtins: for anything else -- a declared
    // aggregate such as `QA_OP_ARRAYAGG` -- the rewrite would feed it a NULL per non-matching row
    // instead of dropping the row, and `array_agg` keeps NULLs. Refuse rather than guess.
    if f.filter.is_some() && !BUILTIN_AGGS.contains(&name.as_str()) {
        return Err(unsupported(format!("FILTER on the non-builtin aggregate {name}")));
    }
    let mut args = Vec::new();
    for a in raw_args {
        match a {
            FunctionArg::Unnamed(FunctionArgExpr::Wildcard) => {}
            FunctionArg::Unnamed(FunctionArgExpr::Expr(e)) => args.push(e.clone()),
            other => return Err(unsupported(format!("aggregate argument {other:?}"))),
        }
    }
    // `COUNT(*) FILTER (WHERE p)` counts matching rows, so it needs *something* to count; `1` is
    // the standard stand-in and makes the rewrite below `COUNT(CASE WHEN p THEN 1 END)`.
    if f.filter.is_some() && args.is_empty() {
        if name != "COUNT" {
            return Err(unsupported(format!("FILTER on argument-less {name}")));
        }
        args.push(Expr::Value(
            SqlValue::Number("1".to_string(), false).with_empty_span(),
        ));
    }
    Ok(Agg { op: name, args, distinct, filter: f.filter.as_deref().cloned() })
}

/// State for lowering expressions over a group's *output* scope (keys first, then aggregate results).
/// Aggregates encountered are accumulated into `funcs` (with their args appended to `preproj`).
///
/// Both scopes this juggles — the pre-aggregation projection and the group output — are binders the
/// frontend introduces, so positions in them become levels only after adding [`Scope::base`]
/// (reachable through `scope`). See that field for why.
struct AggCtx<'a> {
    cat: &'a Catalog,
    scope: &'a Scope, // the FROM (pre-aggregation) scope
    fns: &'a Fns,
    key_vals: Vec<Value>,
    m: usize,
    preproj: Vec<Value>, // initialised to key_vals; aggregate args appended
    funcs: Vec<Value>,   // AggCall JSONs; output position = m + index
}

impl AggCtx<'_> {
    fn key_match(&self, e: &Expr) -> Result<Option<usize>> {
        // An expression containing an aggregate can never *be* a GROUP BY key, and lowering it in
        // the pre-aggregation scope would trip the aggregate-in-scalar-position guard.
        if contains_agg(self.fns, e) {
            return Ok(None);
        }
        let v = lower_expr(self.cat, self.scope, self.fns, e)?;
        Ok(self.key_vals.iter().position(|k| *k == v))
    }

    fn add_agg(&mut self, a: Agg) -> Result<Value> {
        // `agg(x) FILTER (WHERE p)` folds over the rows where `p` holds. There is nothing in the
        // prover's `AggCall` to say so, but for an aggregate that skips NULL inputs the standard
        // rewrite says it anyway: `agg(CASE WHEN p THEN x END)` hands the fold a NULL for every
        // non-matching row, and the `ignoreNulls` flag set below then drops exactly those rows.
        // `p` is evaluated in the pre-aggregation scope, like the aggregate's own arguments.
        // [`agg_of`] has already restricted this to the builtins, whose null-skipping is what makes
        // the rewrite an identity rather than an approximation.
        let filter = match &a.filter {
            Some(p) => Some(lower_bool(self.cat, self.scope, self.fns, p)?),
            None => None,
        };
        let mut operand: Vec<Value> = Vec::new();
        for arg in &a.args {
            let mut v = lower_expr(self.cat, self.scope, self.fns, arg)?;
            if let Some(p) = &filter {
                // A NULL of the argument's own type, so `make_case` finds the branches already in
                // agreement and leaves the NULL uncast -- a cast one would stop testing as null.
                let null_v = json!({ "operator": "NULL", "operand": [], "type": ty_of(&v) });
                v = make_case(vec![p.clone(), v, null_v]);
            }
            let pos = self.preproj.len();
            let ty = ty_of(&v);
            self.preproj.push(v);
            operand.push(json!({ "column": self.scope.base + pos, "type": ty }));
        }
        // The declaration is looked up the same way [`is_agg_call`] found it — qualified, then bare.
        // Keying on `a.op` alone would recognise `public.myagg` as an aggregate and then miss its
        // declared return type, falling through to the argument's type below.
        let declared = fn_decl(self.fns, &a.op, bare_name(&a.op))
            .filter(|d| d.aggregate)
            .map(|d| d.ret.clone());
        let rty = if a.op == "COUNT" {
            "INTEGER".to_string()
        } else if let Some(d) = declared {
            d
        } else if let Some(t) = opaque_agg_ret(&a.op) {
            t.to_string()
        } else if let Some(f) = operand.first() {
            ty_of(f)
        } else {
            "INTEGER".to_string()
        };
        // `ignoreNulls` tells the prover whether to restrict the aggregate's input to rows where
        // every argument is non-NULL. That is exactly the semantics of the builtins when they have
        // arguments: `COUNT(x)`/`SUM(x)`/`AVG`/`MIN`/`MAX` all skip NULL inputs. Emitting `false`
        // here (as Calcite's `ignoreNulls()` does, since it means the unrelated `IGNORE NULLS`
        // window modifier) makes the prover count NULL rows, which collapses `COUNT(x)` into
        // `COUNT(*)` -- a false positive.
        //
        // Two cases must stay `false`:
        //   * no arguments (`COUNT(*)`): the prover applies the filter to the *whole source row*,
        //     which would count only rows with no NULL in any column.
        //   * uninterpreted aggregates, whether declared or from [`OPAQUE_AGGS`]: their null
        //     handling is not something we model (`array_agg` keeps NULLs, `string_agg` drops them),
        //     and `false` is the incomplete-not-unsound direction -- it distinguishes bags that
        //     differ only in NULLs rather than conflating them.
        let ignore_nulls = !operand.is_empty() && BUILTIN_AGGS.contains(&a.op.as_str());
        self.funcs.push(json!({
            "operator": a.op, "operand": operand, "type": rty,
            "distinct": a.distinct, "ignoreNulls": ignore_nulls,
        }));
        Ok(json!({ "column": self.scope.base + self.m + self.funcs.len() - 1, "type": rty }))
    }

    /// Lower an expression that lives in the post-aggregation scope (group keys + aggregate results).
    fn lower_post(&mut self, e: &Expr) -> Result<Value> {
        if is_agg_call(self.fns, e) {
            let a = agg_of(e)?;
            return self.add_agg(a);
        }
        if let Some(j) = self.key_match(e)? {
            return Ok(json!({ "column": self.scope.base + j, "type": ty_of(&self.key_vals[j]) }));
        }
        match e {
            Expr::Nested(i) => self.lower_post(i),
            Expr::Value(v) => lower_value(&v.value),
            Expr::BinaryOp { left, op, right } => {
                use BinaryOperator::*;
                let l = self.lower_post(left)?;
                let r = self.lower_post(right)?;
                let (s, t) = binop(op, &l, &r)?;
                if matches!(op, Eq | NotEq | Lt | Gt | LtEq | GtEq) {
                    Ok(make_cmp(&s, l, r))
                } else if matches!(op, And | Or) {
                    Ok(json!({ "operator": s, "operand": [coerce_bool(l), coerce_bool(r)], "type": t }))
                } else {
                    Ok(make_arith(&s, l, r, &t))
                }
            }
            Expr::UnaryOp { op, expr } => {
                let inner = self.lower_post(expr)?;
                unary(op, inner)
            }
            Expr::IsNull(i) => Ok(json!({ "operator": "IS NULL", "operand": [self.lower_post(i)?], "type": "BOOLEAN" })),
            Expr::IsNotNull(i) => Ok(json!({ "operator": "IS NOT NULL", "operand": [self.lower_post(i)?], "type": "BOOLEAN" })),
            Expr::Cast { expr, data_type, .. } => {
                Ok(lower_cast(self.lower_post(expr)?, data_type))
            }
            Expr::Case { operand, conditions, else_result, .. } => {
                if operand.is_some() {
                    return Err(unsupported("simple CASE in post-aggregate position"));
                }
                let mut ops: Vec<Value> = Vec::new();
                for w in conditions {
                    ops.push(self.lower_post(&w.condition)?);
                    ops.push(self.lower_post(&w.result)?);
                }
                let else_v = match else_result {
                    Some(e) => self.lower_post(e)?,
                    None => json!({ "operator": "NULL", "operand": [], "type": "INTEGER" }),
                };
                ops.push(else_v);
                Ok(make_case(ops))
            }
            Expr::Function(f) => {
                // a scalar function over keys/aggregates (is_agg_call already returned false)
                let (name, bare) = fn_names(f);
                reject_qualified_builtin_agg(&name, &bare)?;
                reject_nondeterministic(&name, &bare)?;
                reject_order_sensitive_agg(&name, &bare)?;
                // SOUNDNESS GUARD, the same as `lower_expr`'s: a set-returning function over
                // aggregates is no more a scalar than one over columns, and lowering it as one
                // understates the row count.
                if SET_RETURNING.contains(&bare.as_str()) {
                    return Err(unsupported(format!("set-returning function {name} in scalar position")));
                }
                let (raw_args, _, _) = fn_args(f)?;
                let mut operand: Vec<Value> = Vec::new();
                for a in raw_args {
                    if let FunctionArg::Unnamed(FunctionArgExpr::Expr(e)) = a {
                        operand.push(self.lower_post(e)?);
                    }
                }
                let ret = fn_ret(self.fns, &name, &bare);
                Ok(json!({ "operator": name, "operand": operand, "type": ret }))
            }
            Expr::Identifier(_) | Expr::CompoundIdentifier(_) => {
                Err(unsupported("column not functionally dependent on GROUP BY"))
            }
            other => Err(unsupported(format!("post-aggregate expression {other:?}"))),
        }
    }
}

/// Refuse a pattern spelled `ALL(..)`, `ANY(..)` or `SOME(..)`.
///
/// sqlparser reads `s LIKE ALL($1)` as a `LIKE` whose pattern is a call to a function named `ALL`
/// (only `LIKE ANY` gets its own flag, refused above). Lowered that way it is one match against one
/// opaque pattern, which a prover reads as strict -- but the quantified form is not:
/// `NULL LIKE ALL('{}')` is TRUE, as an `ALL` over no elements is.
fn refuse_quantified_pattern(op: &str, pattern: &Expr) -> Result<()> {
    if let Expr::Function(f) = pattern {
        if let [part] = f.name.0.as_slice() {
            if let Some(id) = part.as_ident() {
                let q = id.value.to_uppercase();
                if id.quote_style.is_none() && matches!(q.as_str(), "ALL" | "ANY" | "SOME") {
                    return Err(unsupported(format!("{op} {q}(..)")));
                }
            }
        }
    }
    Ok(())
}

/// The absolute column level a lowered expression *is*, if it is a bare column reference.
fn plain_col(v: &Value) -> Option<usize> {
    if v.get("operand").is_some() {
        return None;
    }
    v.get("column").and_then(|c| c.as_u64()).map(|c| c as usize)
}

/// Whether the grouped levels pin down `level` — i.e. whether they contain every column of some
/// declared key of the same binding.
///
/// Postgres accepts a non-grouped column when it is functionally dependent on the GROUP BY, and this
/// is that rule, restricted to the one dependence a `CREATE TABLE` proves: group on a key of a table
/// and each group holds rows from a single row of that table, so every other column of it is constant
/// within the group.
///
/// Both restrictions are load-bearing:
///
///   * **the key's columns must be NOT NULL.** `UNIQUE` alone permits many rows with a NULL key, and
///     `GROUP BY` puts all of them in one group — so on `T(u UNIQUE, b)` holding `{(NULL,1),
///     (NULL,2)}`, `GROUP BY u` is one group and `b` is not constant in it. `PRIMARY KEY` implies
///     NOT NULL, so the common case passes; a nullable `UNIQUE` is refused.
///   * **the key must be grouped on the same binding.** [`Scope::binding_of`] resolves the level to
///     a relation *instance*, so under `t AS a JOIN t AS b`, grouping on `a.id` does not license
///     reading `b.name`.
///
/// An outer join does not break it. If the binding is on the nullable side, an unmatched row has
/// NULL for the whole key *and* for the dependent column; since a real row cannot have a NULL key,
/// the all-NULL group is exactly the unmatched rows and the column is constant (NULL) across it.
fn key_determines(cat: &Catalog, scope: &Scope, grouped: &[usize], level: usize) -> bool {
    let Some((b, _)) = scope.binding_of(level) else { return false };
    let Some(t) = b.table else { return false };
    let tbl = &cat.tables[t];
    tbl.keys.iter().any(|k| {
        !k.is_empty() && k.iter().all(|&ci| !tbl.nullable[ci] && grouped.contains(&(b.offset + ci)))
    })
}

/// Every column level a lowered expression references.
///
/// `{"column": n}` is the IR's only way to name a value from the scope, so collecting that one key
/// over the whole tree cannot miss a reference — including references buried in a subquery, which is
/// the case [`constant_per_group`] needs and which a walk of the *SQL* cannot promise without an arm
/// for every expression variant. The other integer the IR carries, `{"scan": i}`, is a catalog index
/// rather than a level and is correctly ignored.
fn ir_levels(v: &Value, out: &mut Vec<usize>) {
    match v {
        Value::Object(o) => {
            for (k, x) in o {
                match (k.as_str(), x.as_u64()) {
                    ("column", Some(n)) => out.push(n as usize),
                    _ => ir_levels(x, out),
                }
            }
        }
        Value::Array(a) => a.iter().for_each(|x| ir_levels(x, out)),
        _ => {}
    }
}

/// Whether a lowered expression takes a single value within each group.
///
/// [`key_determines`] lifted from a column to an expression: an expression is a function of the
/// columns it reads, so if every one of them is constant within a group then so is it.
fn constant_per_group(cat: &Catalog, scope: &Scope, grouped: &[usize], v: &Value) -> bool {
    let mut levels = Vec::new();
    ir_levels(v, &mut levels);
    levels.iter().all(|&l| {
        // A level no binding in scope covers was minted by a binder *inside* the expression — a
        // subquery's own FROM — so it is bound there and ranges over that subquery's rows, not this
        // query's. Nesting numbers those levels above every binding in scope (a nested query's own
        // columns start at the enclosing context's total width), so they cannot be mistaken for one.
        scope.binding_of(l).is_none()
            || grouped.contains(&l)
            || key_determines(cat, scope, grouped, l)
    })
}

/// Collect the expressions a post-aggregation expression reads, already lowered — the candidates for
/// the key list. Each is a candidate only; [`extend_with_determined`] decides.
///
/// Mirrors [`AggCtx::lower_post`]'s traversal, and the two ways it differs from a plain walk are the
/// point. It stops at an aggregate call, because those arguments are read per input row rather than
/// per group and turning one into a group key would split the very groups it folds over — `GROUP BY
/// t.id ... sum(p.amount)` must not group on `p.amount`. And it descends only through the shapes
/// `lower_post` descends through, so a variant missing here is one `lower_post` refuses anyway: the
/// cost of falling behind it is a refusal, never a key that should not be there.
fn post_columns(cat: &Catalog, scope: &Scope, fns: &Fns, e: &Expr, out: &mut Vec<Value>) {
    if is_agg_call(fns, e) {
        return;
    }
    match e {
        Expr::Identifier(id) => {
            if let Ok(v) = col_ref(scope, None, &id.value) {
                out.push(v);
            }
        }
        Expr::CompoundIdentifier(parts) => {
            if let [q, col] = &parts[..] {
                if let Ok(v) = col_ref(scope, Some(&q.value), &col.value) {
                    out.push(v);
                }
            }
        }
        Expr::Nested(i)
        | Expr::UnaryOp { expr: i, .. }
        | Expr::IsNull(i)
        | Expr::IsNotNull(i)
        | Expr::Cast { expr: i, .. } => post_columns(cat, scope, fns, i, out),
        Expr::BinaryOp { left, right, .. } => {
            post_columns(cat, scope, fns, left, out);
            post_columns(cat, scope, fns, right, out);
        }
        Expr::Case { conditions, else_result, .. } => {
            for w in conditions {
                post_columns(cat, scope, fns, &w.condition, out);
                post_columns(cat, scope, fns, &w.result, out);
            }
            if let Some(x) = else_result {
                post_columns(cat, scope, fns, x, out);
            }
        }
        Expr::Function(f) => {
            if let Ok((args, _, _)) = fn_args(f) {
                for a in args {
                    if let FunctionArg::Unnamed(FunctionArgExpr::Expr(x)) = a {
                        post_columns(cat, scope, fns, x, out);
                    }
                }
            }
        }
        // The subquery-bearing shapes, which `lower_post` has no post-aggregate rule for at all. The
        // dependence lifts from a column to a whole expression unchanged — see [`constant_per_group`]
        // — so offering the expression itself as a key is what lets `lower_post` match it. Whether it
        // *is* determined is decided there; an expression that is not simply stays refused, as
        // `GROUP BY $5, $6` with a projection reading `tags` must.
        //
        // Offered whole rather than descended into: none of the columns inside are reachable
        // individually in the post-group scope, so collecting them would only propose keys that
        // split groups. The list is an allowlist so that adding a shape is a deliberate act; leaving
        // one out costs a refusal, never a wrong key.
        // The guard is because an aggregate of *this* query inside the candidate would be lowered in
        // the wrong scope. One inside the subquery belongs to the subquery, and `contains_agg` stops
        // at that boundary, so `EXISTS (SELECT ... sum(x) ...)` is still offered.
        Expr::Exists { .. }
        | Expr::Subquery(_)
        | Expr::InSubquery { .. }
        | Expr::AnyOp { .. }
        | Expr::AllOp { .. }
            if !contains_agg(fns, e) =>
        {
            if let Ok(v) = lower_expr(cat, scope, fns, e) {
                out.push(v);
            }
        }
        _ => {}
    }
}

/// Append the columns the GROUP BY functionally determines to the key list, so they lower as keys.
///
/// The rewrite is to group on `keys ++ determined` instead of `keys`. Under the dependence that is
/// the same partition — every determined column is already constant within a group, so adding it
/// splits nothing — and it is the only lowering available: the prover's post-group scope holds the
/// keys and the aggregate results, with no way to name a column that is neither.
///
/// Dependence is tested against the *declared* keys only, computed before any column is appended, so
/// the result does not depend on the order candidates are visited.
fn extend_with_determined(cat: &Catalog, scope: &Scope, fns: &Fns, s: &Select, key_vals: &mut Vec<Value>) {
    let grouped: Vec<usize> = key_vals.iter().filter_map(plain_col).collect();
    if grouped.is_empty() {
        return;
    }
    let mut cands: Vec<Value> = Vec::new();
    for it in &s.projection {
        if let Some(e) = item_expr(it) {
            post_columns(cat, scope, fns, e, &mut cands);
        }
    }
    if let Some(h) = &s.having {
        post_columns(cat, scope, fns, h, &mut cands);
    }
    for v in cands {
        if key_vals.contains(&v) {
            continue;
        }
        if constant_per_group(cat, scope, &grouped, &v) {
            key_vals.push(v);
        }
    }
}

/// Lower one `GROUP BY` item, falling back to a select-list alias when the FROM scope has no such
/// column.
///
/// ```text
/// SELECT event_id AS eid, count(*) FROM events GROUP BY eid
/// ```
///
/// Postgres accepts that: a **bare, unqualified** `GROUP BY` name may name an output column. The
/// keys here are lowered against the FROM scope, where an alias the projection introduces does not
/// exist — and it cannot simply be looked up in `out_cols`, because those do not exist yet either
/// (the projection is lowered *after* the grouping, over the post-aggregation scope). So the alias
/// is resolved syntactically, out of `s.projection`, and the expression it names is lowered in its
/// place. That is the same relation `GROUP BY <that expression>` would produce, which is why
/// [`extend_with_determined`] and the identity elision above keep working unchanged: the key holds
/// the expression's value either way, so the projection item carrying the alias still matches it.
///
/// **The FROM scope is tried first, and that ordering is the rule rather than an optimization.**
/// Postgres resolves a `GROUP BY` name as an input column when one exists and only then as an
/// output column, so an input column of the same name has to win.
///
/// Everything else declines rather than guesses:
///
/// * a **qualified** name (`t.x`) never denotes an output column, so the fallback is not taken;
/// * **two select items sharing the alias** is a resolution question with no right answer, refused
///   exactly as [`order_key_index`] refuses the same ambiguity for `ORDER BY`;
/// * an alias over an **aggregate** is rejected by Postgres itself, so lowering it would be
///   lowering a query that does not run;
/// * when the alias path also fails, the **original** error is what is reported — the fallback
///   should not move a row's blocker onto a construct that was never the cause.
fn lower_group_key(
    cat: &Catalog,
    scope: &Scope,
    fns: &Fns,
    s: &Select,
    e: &Expr,
) -> Result<Value> {
    // `GROUP BY 1` is a 1-based position in the select list, not the integer 1, and nothing here
    // resolves it — `group_by_exprs` has no positional arm, so it would lower as the literal and
    // group every row together. There are no such rows in any corpus measured, so this is a guard
    // against a silent wrong answer rather than a feature declined. A non-integer constant is
    // refused by Postgres outright, which lands in the same place.
    if let Expr::Value(v) = e {
        if let SqlValue::Number(n, _) = &v.value {
            return Err(unsupported(format!("GROUP BY position {n}")));
        }
    }
    let err = match lower_expr(cat, scope, fns, e) {
        Ok(v) => return Ok(v),
        Err(err) => err,
    };
    let Expr::Identifier(id) = e else { return Err(err) };
    match alias_target(fns, s, &id.value)? {
        Some(target) => lower_expr(cat, scope, fns, target),
        None => Err(err),
    }
}

/// The expression a select-list alias names, for [`lower_group_key`]'s fallback.
///
/// `Ok(None)` means no item carries the alias — the caller keeps its original error. `Err` is for
/// the two cases where an item does carry it but using it would be wrong.
fn alias_target<'a>(fns: &Fns, s: &'a Select, name: &str) -> Result<Option<&'a Expr>> {
    let name = name.to_lowercase();
    let mut found = s.projection.iter().filter_map(|it| match it {
        SelectItem::ExprWithAlias { expr, alias } if alias.value.to_lowercase() == name => {
            Some(expr)
        }
        _ => None,
    });
    match (found.next(), found.next()) {
        (Some(e), None) if contains_agg(fns, e) => {
            Err(schema(format!("aggregate in GROUP BY key {name}")))
        }
        (Some(e), None) => Ok(Some(e)),
        (Some(_), Some(_)) => Err(schema(format!("ambiguous GROUP BY key {name}"))),
        _ => Ok(None),
    }
}

fn lower_aggregate(cat: &Catalog, scope: &Scope, fns: &Fns, rel: Value, s: &Select) -> Result<(Value, OutCols)> {
    let keys = group_by_exprs(s)?;
    let mut key_vals: Vec<Value> = keys
        .iter()
        .map(|e| lower_group_key(cat, scope, fns, s, e))
        .collect::<Result<Vec<_>>>()?;
    // Must happen before `m` is read: the aggregate-result levels are numbered from the end of the
    // key list, so a key appended later would shift every one of them.
    extend_with_determined(cat, scope, fns, s, &mut key_vals);
    let m = key_vals.len();
    let mut ag = AggCtx { cat, scope, fns, key_vals: key_vals.clone(), m, preproj: key_vals.clone(), funcs: Vec::new() };

    // Lower SELECT items and HAVING over the post-aggregation scope (both may introduce aggregates).
    let mut out_cols: OutCols = Vec::new();
    let mut targets: Vec<Value> = Vec::new();
    for (idx, it) in s.projection.iter().enumerate() {
        let e = item_expr(it).ok_or_else(|| unsupported(format!("aggregate projection item {it:?}")))?;
        let v = ag.lower_post(e)?;
        let name = match it {
            SelectItem::ExprWithAlias { alias, .. } => alias.value.to_lowercase(),
            _ => expr_name(e, idx),
        };
        out_cols.push((name, ty_of(&v)));
        targets.push(v);
    }
    let having = match &s.having {
        Some(h) => Some(coerce_bool(ag.lower_post(h)?)),
        None => None,
    };

    // Build Group over the pre-projection (keys ++ aggregate args).
    let source = if ag.preproj.is_empty() {
        rel
    } else {
        json!({ "project": { "target": ag.preproj, "source": rel } })
    };
    let gkeys: Vec<Value> = key_vals
        .iter()
        .enumerate()
        .map(|(i, v)| json!({ "column": scope.base + i, "type": ty_of(v) }))
        .collect();
    let mut result = json!({ "group": { "keys": gkeys, "function": ag.funcs, "source": source } });

    if let Some(cond) = having {
        result = json!({ "filter": { "condition": cond, "source": result } });
    }

    // Elide the top projection when SELECT is exactly the group output in order (matches Calcite).
    let n_out = m + result_group_func_count(&result);
    let is_identity = targets.len() == n_out
        && targets.iter().enumerate().all(|(i, t)| {
            t.get("column").and_then(|c| c.as_u64()) == Some((scope.base + i) as u64)
                && t.get("operand").is_none()
        });
    if !is_identity {
        result = json!({ "project": { "target": targets, "source": result } });
    }
    Ok((result, out_cols))
}

/// Number of aggregate functions in a group / filter(group) result (for the identity-projection check).
fn result_group_func_count(v: &Value) -> usize {
    let g = v.get("group").or_else(|| v.get("filter").and_then(|f| f.get("source")).and_then(|s| s.get("group")));
    g.and_then(|g| g.get("function")).and_then(|f| f.as_array()).map(|a| a.len()).unwrap_or(0)
}

/// Apply a unary operator to an already-lowered operand.
fn unary(op: &UnaryOperator, inner: Value) -> Result<Value> {
    match op {
        UnaryOperator::Plus => Ok(inner),
        UnaryOperator::Minus => {
            let ty = ty_of(&inner);
            Ok(json!({ "operator": "-", "operand": [inner], "type": ty }))
        }
        UnaryOperator::Not => Ok(json!({ "operator": "NOT", "operand": [inner], "type": "BOOLEAN" })),
        other => Err(unsupported(format!("unary op {other:?}"))),
    }
}

/// Lower a scalar expression to an `Expr` Value.
fn lower_expr(cat: &Catalog, scope: &Scope, fns: &Fns, e: &Expr) -> Result<Value> {
    match e {
        Expr::Nested(inner) => lower_expr(cat, scope, fns, inner),
        Expr::Identifier(id) => col_ref(scope, None, &id.value),
        Expr::CompoundIdentifier(parts) => {
            let col = parts.last().unwrap();
            let qual = if parts.len() >= 2 { Some(parts[parts.len() - 2].value.as_str()) } else { None };
            col_ref(scope, qual, &col.value)
        }
        Expr::Value(v) => lower_value(&v.value),
        Expr::Function(f) => {
            if f.over.is_some() {
                return Err(unsupported("window function (OVER)"));
            }
            // SOUNDNESS GUARD: an aggregate reaching the scalar path would be lowered as a per-row
            // function, quietly turning one output row into one row per input row. `contains_agg`
            // routes aggregates to the Group path before we get here, so anything still arriving is
            // a case it does not model (e.g. an aggregate in WHERE, or nested in a form it does not
            // walk) and must be refused rather than mis-lowered.
            if is_agg_call(fns, e) {
                let name = obj_name(&f.name).to_uppercase();
                return Err(unsupported(format!("aggregate {name} in scalar position")));
            }
            let (raw_args, _distinct, has_order) = fn_args(f)?;
            if f.filter.is_some() || has_order {
                return Err(unsupported("function FILTER / ORDER BY"));
            }
            let (name, bare) = fn_names(f);
            reject_qualified_builtin_agg(&name, &bare)?;
            reject_nondeterministic(&name, &bare)?;
            reject_order_sensitive_agg(&name, &bare)?;
            // SOUNDNESS GUARD: see [`SET_RETURNING`] — these are not scalars and lowering them as
            // one would understate the row count. Matched on the *bare* name: `public.unnest(x)` is
            // still `unnest`, and widening a refusal can only ever cost completeness.
            if SET_RETURNING.contains(&bare.as_str()) {
                return Err(unsupported(format!("set-returning function {name} in scalar position")));
            }
            let mut operand: Vec<Value> = Vec::new();
            for a in raw_args {
                if let FunctionArg::Unnamed(FunctionArgExpr::Expr(e)) = a {
                    operand.push(lower_expr(cat, scope, fns, e)?);
                }
            }
            Ok(json!({ "operator": name, "operand": operand, "type": fn_ret(fns, &name, &bare) }))
        }
        // Row-constructor comparison: `(a, b) = (x, y)`. The standard defines row `=` as the
        // conjunction of the pairwise comparisons, three-valued logic included — true iff every
        // pair is true, false iff some pair is false, unknown otherwise — which is exactly what
        // `AND` over the pairwise `=` yields. Row `<>` is defined as the negation of row `=`, so it
        // is the same expansion under a `NOT`. The ordering comparisons are lexicographic rather
        // than pairwise, so they get no expansion here and are refused.
        Expr::BinaryOp { left, op, right }
            if matches!(**left, Expr::Tuple(_)) || matches!(**right, Expr::Tuple(_)) =>
        {
            let (le, re) = match (left.as_ref(), right.as_ref()) {
                (Expr::Tuple(l), Expr::Tuple(r)) => (l, r),
                _ => return Err(unsupported("row comparison against a non-row operand")),
            };
            if le.len() != re.len() {
                return Err(schema(format!(
                    "row comparison arity: {} column(s) on the left, {} on the right",
                    le.len(),
                    re.len()
                )));
            }
            let negated = match op {
                BinaryOperator::Eq => false,
                BinaryOperator::NotEq => true,
                other => return Err(unsupported(format!("row comparison with {other:?}"))),
            };
            let ls: Vec<Value> =
                le.iter().map(|x| lower_expr(cat, scope, fns, x)).collect::<Result<_>>()?;
            let m = row_match(&ls, re, cat, scope, fns)?;
            Ok(if negated { not_bool(m) } else { m })
        }
        Expr::BinaryOp { left, op, right } => {
            use BinaryOperator::*;
            let l = lower_expr(cat, scope, fns, left)?;
            let r = lower_expr(cat, scope, fns, right)?;
            let (opstr, ty) = binop(op, &l, &r)?;
            if matches!(op, Eq | NotEq | Lt | Gt | LtEq | GtEq) {
                Ok(make_cmp(&opstr, l, r))
            } else if matches!(op, And | Or) {
                Ok(json!({ "operator": opstr, "operand": [coerce_bool(l), coerce_bool(r)], "type": ty }))
            } else {
                Ok(make_arith(&opstr, l, r, &ty))
            }
        }
        Expr::UnaryOp { op, expr } => {
            let inner = lower_expr(cat, scope, fns, expr)?;
            unary(op, inner)
        }
        Expr::IsNull(inner) => {
            Ok(json!({ "operator": "IS NULL", "operand": [lower_expr(cat, scope, fns, inner)?], "type": "BOOLEAN" }))
        }
        Expr::IsNotNull(inner) => {
            Ok(json!({ "operator": "IS NOT NULL", "operand": [lower_expr(cat, scope, fns, inner)?], "type": "BOOLEAN" }))
        }
        // The three-valued `IS <truth value>` tests. The prover interprets `IS TRUE` as "this
        // expression evaluates to true" and `IS NOT TRUE` as its complement (so NULL satisfies
        // `IS NOT TRUE`), which is exactly SQL. The other four are not interpreted natively but
        // are definable from the two that are, without losing the NULL case:
        //   `x IS FALSE`       == `(NOT x) IS TRUE`      (NOT NULL is NULL, so NULL fails both)
        //   `x IS NOT FALSE`   == `(NOT x) IS NOT TRUE`
        //   `x IS UNKNOWN`     == `x IS NULL`            (UNKNOWN *is* the NULL boolean)
        //   `x IS NOT UNKNOWN` == `x IS NOT NULL`
        Expr::IsTrue(i) | Expr::IsNotTrue(i) | Expr::IsFalse(i) | Expr::IsNotFalse(i) => {
            let v = coerce_bool(lower_expr(cat, scope, fns, i)?);
            let (negate_operand, op) = match e {
                Expr::IsTrue(_) => (false, "IS TRUE"),
                Expr::IsNotTrue(_) => (false, "IS NOT TRUE"),
                Expr::IsFalse(_) => (true, "IS TRUE"),
                _ => (true, "IS NOT TRUE"),
            };
            let v = if negate_operand { not_bool(v) } else { v };
            Ok(json!({ "operator": op, "operand": [v], "type": "BOOLEAN" }))
        }
        Expr::IsUnknown(i) => {
            Ok(json!({ "operator": "IS NULL", "operand": [lower_expr(cat, scope, fns, i)?], "type": "BOOLEAN" }))
        }
        Expr::IsNotUnknown(i) => {
            Ok(json!({ "operator": "IS NOT NULL", "operand": [lower_expr(cat, scope, fns, i)?], "type": "BOOLEAN" }))
        }
        Expr::IsDistinctFrom(a, b) => {
            Ok(make_cmp("IS DISTINCT FROM", lower_expr(cat, scope, fns, a)?, lower_expr(cat, scope, fns, b)?))
        }
        Expr::IsNotDistinctFrom(a, b) => {
            Ok(make_cmp("IS NOT DISTINCT FROM", lower_expr(cat, scope, fns, a)?, lower_expr(cat, scope, fns, b)?))
        }
        // x IN (a, b, ...) -> OR(x = a, ...);  (l1,l2) IN ((a1,a2),...) -> OR(AND(l1=a1, l2=a2), ...)
        Expr::InList { expr, list, negated } => {
            let join = if *negated { "AND" } else { "OR" };
            let cmp = if *negated { "<>" } else { "=" };
            let mut terms: Vec<Value> = Vec::new();
            if let Expr::Tuple(lhs_elems) = expr.as_ref() {
                let ls: Vec<Value> =
                    lhs_elems.iter().map(|e| lower_expr(cat, scope, fns, e)).collect::<Result<Vec<_>>>()?;
                for item in list {
                    let relems = match item {
                        Expr::Tuple(r) => r,
                        _ => return Err(unsupported("row IN list item must be a tuple")),
                    };
                    if relems.len() != ls.len() {
                        return Err(unsupported("row IN arity mismatch"));
                    }
                    let m = row_match(&ls, relems, cat, scope, fns)?;
                    terms.push(if *negated { not_bool(m) } else { m });
                }
            } else {
                let lhs = lower_expr(cat, scope, fns, expr)?;
                for item in list {
                    terms.push(make_cmp(cmp, lhs.clone(), lower_expr(cat, scope, fns, item)?));
                }
            }
            Ok(if terms.len() == 1 {
                terms.into_iter().next().unwrap()
            } else {
                json!({ "operator": join, "operand": terms, "type": "BOOLEAN" })
            })
        }
        Expr::Case { operand, conditions, else_result, .. } => {
            let mut ops: Vec<Value> = Vec::new();
            for w in conditions {
                let cond = match operand {
                    Some(scrut) => make_cmp(
                        "=",
                        lower_expr(cat, scope, fns, scrut)?,
                        lower_expr(cat, scope, fns, &w.condition)?,
                    ),
                    None => lower_expr(cat, scope, fns, &w.condition)?,
                };
                ops.push(cond);
                ops.push(lower_expr(cat, scope, fns, &w.result)?);
            }
            let else_v = match else_result {
                Some(e) => lower_expr(cat, scope, fns, e)?,
                None => json!({ "operator": "NULL", "operand": [], "type": "INTEGER" }),
            };
            ops.push(else_v);
            Ok(make_case(ops))
        }
        // LIKE, ILIKE and SIMILAR TO are uninterpreted boolean operators, kept distinct by name: the
        // prover has no rule for any of them, so it treats each as an uninterpreted function of its
        // arguments and never unifies one with another. `ESCAPE` and the Snowflake `LIKE ANY` form
        // change the matching semantics and are not modelled, so they are refused rather than dropped.
        Expr::Like { negated, expr, pattern, escape_char, any }
        | Expr::ILike { negated, expr, pattern, escape_char, any } => {
            let op = if matches!(e, Expr::ILike { .. }) { "ILIKE" } else { "LIKE" };
            if *any {
                return Err(unsupported(format!("{op} ANY")));
            }
            refuse_quantified_pattern(op, pattern)?;
            lower_match(cat, scope, fns, op, *negated, expr, pattern, escape_char.is_some())
        }
        Expr::SimilarTo { negated, expr, pattern, escape_char } => {
            refuse_quantified_pattern("SIMILAR TO", pattern)?;
            lower_match(cat, scope, fns, "SIMILAR TO", *negated, expr, pattern, escape_char.is_some())
        }
        Expr::Between { expr, negated, low, high } => {
            let e = lower_expr(cat, scope, fns, expr)?;
            let lo = lower_expr(cat, scope, fns, low)?;
            let hi = lower_expr(cat, scope, fns, high)?;
            let ge = make_cmp(">=", e.clone(), lo);
            let le = make_cmp("<=", e, hi);
            Ok(if *negated {
                json!({ "operator": "OR", "operand": [not_bool(ge), not_bool(le)], "type": "BOOLEAN" })
            } else {
                json!({ "operator": "AND", "operand": [ge, le], "type": "BOOLEAN" })
            })
        }
        Expr::Cast { expr, data_type, .. } => {
            Ok(lower_cast(lower_expr(cat, scope, fns, expr)?, data_type))
        }
        Expr::InSubquery { expr, subquery, negated } => {
            lower_in_subquery(cat, scope, fns, expr, subquery, *negated)
        }
        // `x = ANY(...)` / `x <> ALL(...)` -- see [`lower_quantified`], which also explains why the
        // other comparison operators are refused.
        Expr::AnyOp { left, compare_op, right, .. } => {
            lower_quantified(cat, scope, fns, left, compare_op, right, false)
        }
        Expr::AllOp { left, compare_op, right } => {
            lower_quantified(cat, scope, fns, left, compare_op, right, true)
        }
        Expr::Exists { subquery, negated } => {
            let sub = lower_query_ctx(cat, fns, subquery, &scope.binds)?.0;
            let v = json!({ "operator": "EXISTS", "operand": [], "query": sub, "type": "BOOLEAN" });
            Ok(if *negated { not_bool(v) } else { v })
        }
        // A scalar subquery: the value of the single column of its single row (NULL if it returns no
        // row). Emitted as `$SCALAR_QUERY` -- the spelling Calcite's parser uses for the same thing --
        // which the prover reads as a higher-order operator: an uninterpreted value keyed on the
        // *normalised* subquery relation and the outer row.
        //
        // Sound in both directions. Two of these collapse to one value exactly when their relations
        // normalise equal, and equal relations denote the same bag, hence the same scalar. Otherwise
        // they stay unrelated, so nothing is equated by accident. And because the value is a fresh
        // universally-quantified variable, a proof that holds for every value of it holds for the one
        // SQL actually produces -- which is why the missing 0-row/NULL and >1-row/error cases cost
        // completeness here rather than soundness.
        //
        // It is *more* conservative than `EXISTS`/`IN`, which the prover special-cases into
        // interpreted logic over the relation. This one lands on the generic memoised branch
        // (`normal.rs:790`), whose key includes the whole in-scope substitution vector -- so two sides
        // that differ only in the *order* of their bindings get distinct variables even where the
        // relations normalise equal. Commuting a join under a correlated scalar subquery is the
        // visible case: provable with `EXISTS`, not provable with this. Only ever loses proofs.
        Expr::Subquery(q) => {
            let (rel, cols) = lower_query_ctx(cat, fns, q, &scope.binds)?;
            if cols.len() != 1 {
                return Err(schema(format!(
                    "scalar subquery selects {} columns, expected 1",
                    cols.len()
                )));
            }
            Ok(json!({
                "operator": "$SCALAR_QUERY", "operand": [], "query": rel, "type": cols[0].1
            }))
        }
        other => Err(unsupported(format!("expr: {other:?}"))),
    }
}

/// Lower an expression in a boolean (predicate) context: recurse through AND/OR/NOT so every leaf is
/// lowered as a predicate, coercing non-boolean leaf operators to BOOLEAN.
fn lower_bool(cat: &Catalog, scope: &Scope, fns: &Fns, e: &Expr) -> Result<Value> {
    use BinaryOperator::{And, Or};
    match e {
        Expr::Nested(i) => lower_bool(cat, scope, fns, i),
        Expr::BinaryOp { left, op, right } if matches!(op, And | Or) => {
            let l = lower_bool(cat, scope, fns, left)?;
            let r = lower_bool(cat, scope, fns, right)?;
            let opstr = if matches!(op, And) { "AND" } else { "OR" };
            Ok(json!({ "operator": opstr, "operand": [l, r], "type": "BOOLEAN" }))
        }
        Expr::UnaryOp { op: UnaryOperator::Not, expr } => Ok(not_bool(lower_bool(cat, scope, fns, expr)?)),
        _ => Ok(coerce_bool(lower_expr(cat, scope, fns, e)?)),
    }
}

/// `expr IN (subquery)`, and its negation. Non-correlated and correlated alike: the subquery sees
/// the current row scope as outer.
///
/// The left side may be a row constructor: `(a, b) IN (SELECT x, y FROM t)`. The prover handles that
/// natively — it zips the operands with the subquery's output columns and conjoins the equalities —
/// but it *asserts* the two have the same width, so a mismatch panics it. Check the arity here and
/// refuse instead.
///
/// Shared with [`lower_quantified`], because `x = ANY (SELECT ..)` is not merely equivalent to this,
/// it is the same predicate spelled differently — so it had better lower to the same IR.
fn lower_in_subquery(
    cat: &Catalog,
    scope: &Scope,
    fns: &Fns,
    expr: &Expr,
    subquery: &Query,
    negated: bool,
) -> Result<Value> {
    let lhs: Vec<Value> = match expr {
        Expr::Tuple(elems) => {
            elems.iter().map(|x| lower_expr(cat, scope, fns, x)).collect::<Result<_>>()?
        }
        one => vec![lower_expr(cat, scope, fns, one)?],
    };
    let (sub, sub_cols) = lower_query_ctx(cat, fns, subquery, &scope.binds)?;
    if sub_cols.len() != lhs.len() {
        return Err(schema(format!(
            "IN subquery arity: {} column(s) on the left, {} selected",
            lhs.len(),
            sub_cols.len()
        )));
    }
    // The prover compares each left operand with the subquery's column as it stands, with no
    // coercion of its own, so a DATE against a TIMESTAMP column would compare two units.
    let lhs: Vec<Value> = lhs
        .into_iter()
        .zip(&sub_cols)
        .map(|(x, (_, t))| coerce_in_operand(x, t).map_err(|m| unsupported(format!("IN subquery: {m}"))))
        .collect::<Result<_>>()?;
    let v = json!({ "operator": "IN", "operand": lhs, "query": sub, "type": "BOOLEAN" });
    Ok(if negated { not_bool(v) } else { v })
}

/// `x = ANY(rhs)` and `x <> ALL(rhs)`, in each of the operand shapes Postgres allows on the right.
///
/// ## Why only those two comparison operators
///
/// Postgres allows any comparison under either quantifier, and the prover looks like it agrees: it
/// reads an operator of the form `"<cmp> <quant>"` off a relation-valued node and evaluates it with
/// `quant_cmp`, which implements the three-valued truth table exactly (`normal.rs:517`). But for the
/// *ordered* comparisons that function reaches `fn cmp`, which opens with
/// `assert!(matches!(ty, Integer | Real | String))` and takes no type guard on the way in — unlike
/// the plain binary-comparison path, which checks the operand type before dispatching. So
/// `d > ANY (SELECT ..)` over a DATE or VARBINARY column does not cost a proof, it *panics the
/// prover*. `= ANY` and `<> ALL` take the equality branch instead, which goes through `self.equal`
/// and is total.
///
/// The remaining two pairings, `= ALL` and `<> ANY`, take that same total equality branch and so
/// are not a panic hazard — but neither is `IN`, so they would need the relation-valued form, which
/// nothing in this frontend emits yet. They are refused for want of a case to justify testing it.
///
/// Refusing them costs nothing measurable: in practice a quantified comparison is essentially always
/// `= ANY` or `<> ALL`, and both of those are handled.
///
/// ## An aggregate underneath one is still refused
///
/// [`contains_agg`] does not walk into these nodes, so `count(*) = ANY(..)` on its own does not mark
/// the query as aggregated. That is a coverage gap, not the [`OPAQUE_AGGS`] hazard again — both ways
/// out of it are refusals, never a demotion. On the scalar path the aggregate reaches [`lower_expr`]'s
/// function arm and is refused as "in scalar position" (pinned by a test); where the query is
/// aggregated for some other reason, the expression reaches `lower_post`, which has no arm for these
/// and refuses too (one corpus case does exactly that).
fn lower_quantified(
    cat: &Catalog,
    scope: &Scope,
    fns: &Fns,
    left: &Expr,
    op: &BinaryOperator,
    right: &Expr,
    all: bool,
) -> Result<Value> {
    let quant = if all { "ALL" } else { "ANY" };
    if !matches!((all, op), (false, BinaryOperator::Eq) | (true, BinaryOperator::NotEq)) {
        return Err(unsupported(format!(
            "{op} {quant} (only `= ANY` and `<> ALL` are lowered)"
        )));
    }
    // Parentheses around the operand are syntax: `ANY((SELECT ..))` is `ANY(SELECT ..)`.
    let mut rhs = right;
    while let Expr::Nested(inner) = rhs {
        rhs = inner;
    }
    match rhs {
        // `x = ANY (SELECT ..)` *is* `x IN (SELECT ..)`, and `x <> ALL (SELECT ..)` is its negation
        // — the same three-valued truth table, not just agreement on non-NULL input. The prover
        // reaches identical logic either way (`IN` is literally `quant_cmp("SOME", "=", ..)`), and
        // going through the same function here makes the two spellings produce identical IR, which
        // is what lets a rewrite between them be proved.
        Expr::Subquery(q) => lower_in_subquery(cat, scope, fns, left, q, all),

        // `x = ANY (ARRAY[a, b, c])` -> `(x = a) OR (x = b) OR (x = c)`, and ALL -> AND over `<>`.
        // Exact, NULLs included: three-valued OR is TRUE when some disjunct is, NULL when none is
        // and one is NULL, and FALSE otherwise — ANY's truth table verbatim, and dually for ALL.
        // The empty array lands on the connective's identity, and SQL agrees with that too: `= ANY`
        // of nothing is FALSE, `<> ALL` of nothing is TRUE.
        //
        // This is the one shape the Python preprocessor already rewrites (`desugar_any_all`), so it
        // never survives to reach us today. It is here so that stage does not have to be ported.
        Expr::Array(arr) => {
            let l = lower_expr(cat, scope, fns, left)?;
            let cmp = if all { "<>" } else { "=" };
            let elems: Vec<Value> =
                arr.elem.iter().map(|e| lower_expr(cat, scope, fns, e)).collect::<Result<_>>()?;
            // The expansion compares `x` with each *element*, which is only what `ANY` does when
            // every element is a scalar: over `ARRAY[t.tags]`, with `tags` an array, `ANY` ranges
            // over the leaves of the two-dimensional result, not over `tags` itself. Arrays lower
            // to the opaque VARBINARY, as do other values that are not arrays, so a column or an
            // expression of opaque type is refused rather than guessed at. A parameter or a literal
            // is a scalar whatever its inferred type: under the binding contract each `$N` is one
            // value, typed by what it is compared with.
            // By this point `casts::rewrite` has spelled each `$N` as a call `qpN(0)`.
            let leaf = |e: &Expr| {
                let mut e = e;
                while let Expr::Nested(inner) = e {
                    e = inner;
                }
                match e {
                    Expr::Value(_) => true,
                    Expr::Function(f) => {
                        let n = obj_name(&f.name).to_lowercase();
                        n.strip_prefix("qp").is_some_and(|d| !d.is_empty() && d.bytes().all(|b| b.is_ascii_digit()))
                    }
                    _ => false,
                }
            };
            if arr.elem.iter().zip(&elems).any(|(e, v)| !leaf(e) && ty_of(v) == "VARBINARY") {
                return Err(unsupported(format!(
                    "{op} {quant} over an ARRAY[..] with an element of opaque type"
                )));
            }
            let terms: Vec<Value> = elems.into_iter().map(|v| make_cmp(cmp, l.clone(), v)).collect();
            Ok(match terms.len() {
                0 => {
                    let lit = if all { "TRUE" } else { "FALSE" };
                    json!({ "operator": lit, "operand": [], "type": "BOOLEAN" })
                }
                1 => terms.into_iter().next().unwrap(),
                _ => {
                    let join = if all { "AND" } else { "OR" };
                    json!({ "operator": join, "operand": terms, "type": "BOOLEAN" })
                }
            })
        }

        // Everything else — an array-valued parameter, an array-typed column, `ARRAY(..)` — has no
        // element list to expand and no relation to quantify over. It becomes an uninterpreted
        // boolean symbol applied to the two operands, the same treatment `LIKE` gets.
        //
        // Sound because it is a *function*: `x = ANY(A)` is determined by `x` and `A` alone, so the
        // real semantics is one of the interpretations the prover quantifies over, and whatever it
        // proves for all of them holds for that one. Two things have to be part of the symbol's
        // identity for that to survive, and both are in the name: the comparison operator and the
        // quantifier, so `= ANY` can never unify with `<> ALL`.
        //
        // The prover leaves the symbol genuinely free. `shared.rs:app` declares it over the *option*
        // sorts and asserts no axiom about it — in particular not the strict NULL propagation
        // `null.rs` builds into the operators it does model. That distinction is load-bearing here,
        // because `= ANY` is not strict: `NULL = ANY(ARRAY[])` is FALSE, not NULL. A strictness
        // axiom would have been an unsound assumption about this symbol; there isn't one.
        _ => {
            let l = lower_expr(cat, scope, fns, left)?;
            let r = lower_expr(cat, scope, fns, rhs)?;
            Ok(json!({
                "operator": format!("{op} {quant}"), "operand": [l, r], "type": "BOOLEAN"
            }))
        }
    }
}

/// Positive row match: `AND(l_i = r_i)` for a row-constructor comparison.
fn row_match(ls: &[Value], relems: &[Expr], cat: &Catalog, scope: &Scope, fns: &Fns) -> Result<Value> {
    let mut eqs: Vec<Value> = Vec::new();
    for (l, re) in ls.iter().zip(relems) {
        eqs.push(make_cmp("=", l.clone(), lower_expr(cat, scope, fns, re)?));
    }
    Ok(if eqs.len() == 1 {
        eqs.into_iter().next().unwrap()
    } else {
        json!({ "operator": "AND", "operand": eqs, "type": "BOOLEAN" })
    })
}

/// Lower one of the pattern-matching predicates to its named uninterpreted operator.
#[allow(clippy::too_many_arguments)]
fn lower_match(
    cat: &Catalog,
    scope: &Scope,
    fns: &Fns,
    op: &str,
    negated: bool,
    expr: &Expr,
    pattern: &Expr,
    has_escape: bool,
) -> Result<Value> {
    if has_escape {
        return Err(unsupported(format!("{op} ... ESCAPE")));
    }
    let l = lower_expr(cat, scope, fns, expr)?;
    let p = lower_expr(cat, scope, fns, pattern)?;
    let v = json!({ "operator": op, "operand": [l, p], "type": "BOOLEAN" });
    Ok(if negated { not_bool(v) } else { v })
}

fn col_ref(scope: &Scope, qual: Option<&str>, name: &str) -> Result<Value> {
    // Under an outer `JOIN ... USING`, an unqualified merged name is the preserved side's value
    // (`COALESCE` of both, for FULL) -- not whichever binding resolution happens to reach first.
    if scope.merged_conflict(qual, name) {
        return Err(unsupported(format!("unqualified {name} merged by an outer JOIN ... USING")));
    }
    match scope.try_resolve(qual, name) {
        Some((idx, ty)) => Ok(json!({ "column": idx, "type": ty })),
        None => Err(schema(format!(
            "unresolved column {}{} (correlated subquery or unknown column)",
            qual.map(|q| format!("{q}.")).unwrap_or_default(),
            name
        ))),
    }
}

fn lower_value(v: &SqlValue) -> Result<Value> {
    use SqlValue::*;
    Ok(match v {
        Number(n, _) => {
            let ty = if n.contains('.') { "REAL" } else { "INTEGER" };
            json!({ "operator": n, "operand": [], "type": ty })
        }
        SingleQuotedString(s) | DoubleQuotedString(s) | NationalStringLiteral(s) => {
            json!({ "operator": s, "operand": [], "type": "VARCHAR" })
        }
        Boolean(b) => {
            json!({ "operator": if *b { "TRUE" } else { "FALSE" }, "operand": [], "type": "BOOLEAN" })
        }
        Null => json!({ "operator": "NULL", "operand": [], "type": "INTEGER" }),
        other => return Err(unsupported(format!("literal {other:?}"))),
    })
}

/// Operator string and result type for a binary operator (operand coercion is applied by the caller
/// for comparisons via [`make_cmp`]).
fn binop(op: &BinaryOperator, l: &Value, r: &Value) -> Result<(String, String)> {
    use BinaryOperator::*;
    let s = match op {
        Eq => "=",
        NotEq => "<>",
        Lt => "<",
        Gt => ">",
        LtEq => "<=",
        GtEq => ">=",
        Plus => "+",
        Minus => "-",
        Multiply => "*",
        Divide => "/",
        Modulo => "%",
        And => "AND",
        Or => "OR",
        StringConcat => "||",
        other => return Err(unsupported(format!("binary op {other:?}"))),
    };
    let ty = match op {
        Eq | NotEq | Lt | Gt | LtEq | GtEq | And | Or => "BOOLEAN".to_string(),
        StringConcat => "VARCHAR".to_string(),
        _ if ty_of(l) == "REAL" || ty_of(r) == "REAL" => "REAL".to_string(),
        _ => "INTEGER".to_string(),
    };
    Ok((s.to_string(), ty))
}
