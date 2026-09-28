// Unless explicitly stated otherwise all files in this repository are licensed under the
// Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/).
// Copyright 2026-Present Datadog, Inc.

//! Equivalence-preserving rewrites applied to the parsed tree before anything reads it.
//!
//! Two kinds live here, and the distinction matters for how each is justified:
//!
//! * [`fix_precedence`] guards against a **parser bug**: it refuses the trees where what sqlparser
//!   built is not what Postgres would, because lowering those is the unsound option.
//! * [`strip_in_exists_distinct`] and [`unnest_in_to_any`] are **normalizations**. The tree is
//!   correct and the rewrite changes it anyway, to a form the prover has an easier time with. Their
//!   justification has to be an equivalence argument, and every precondition of that argument has to
//!   be a guard in the code — because the prover then proves a theorem about the *rewritten* pair
//!   and the verdict is reported for the original. An unsound normalization is therefore invisible:
//!   it does not fail, it lies.
//!
//! ## The parser bugs
//!
//! There are two, they have the same shape — a right operand that swallows everything to its right —
//! and they are fixed in two different places, which is worth stating up front.
//!
//! The second one is not repaired here at all: sqlparser's `GenericDialect` gives the Postgres JSON
//! operators no precedence, so `payload ->> 'k' = 'v'` parses as `payload ->> ('k' = 'v')`. That is
//! fixed by parsing with the right dialect (see [`DIALECT`][crate::DIALECT]) rather than by rewriting,
//! because unlike the one below it is not present in every dialect. [`demote_operators`] is what would
//! have carried the mis-parse into the IR, so the pinning test lives beside it.
//!
//! ### The one this module guards
//!
//! sqlparser 0.62 parsed the right operand of `IS [NOT] DISTINCT FROM` at precedence 0, so it
//! swallowed everything to its right:
//!
//! ```text
//! a IS DISTINCT FROM 1 AND b = 2
//!   0.62 parsed   IsDistinctFrom(a, And(1, Eq(b, 2)))     -- a IS DISTINCT FROM (1 AND b = 2)
//!   meaning       And(IsDistinctFrom(a, 1), Eq(b, 2))
//! ```
//!
//! 0.63 parses it at the `IS` family's own precedence, and this pass used to splice the first tree
//! back into the second. It now only refuses: a rewrite that no parser output exercises any more is
//! an untested rewrite of a soundness-relevant tree, and an honest refusal is worth more. This
//! matters beyond a wrong answer: [`lower`][crate::lower] lowers `IsDistinctFrom` faithfully, so a
//! mis-parse hands the prover a *different predicate* than the query states, and two
//! genuinely-inequivalent queries can lower to two equivalent IRs. That is the one way to get a false
//! proof out of a sound prover, so this runs before the catalog is built.
//!
//! Three shapes are refused:
//!
//! * `IS [NOT] DISTINCT FROM` over a bare `AND`/`OR` — the 0.62 mis-parse, should it ever come back.
//! * One `IS` operator directly over another, in either order. PostgreSQL declares the family
//!   non-associative (`%nonassoc IS` in its grammar), so `a IS DISTINCT FROM b IS NULL` is a syntax
//!   error there, whichever way sqlparser happens to nest it.
//! * `IS [NOT] DISTINCT FROM NOT x`, which was never rewritten and is still not.
//!
//! Explicit parentheses are safe: sqlparser keeps them as [`Expr::Nested`], so
//! `a IS DISTINCT FROM (1 AND b)` and `(a IS DISTINCT FROM b) IS NULL` are left alone.

use core::ops::ControlFlow;

use std::collections::hash_map::Entry;
use std::collections::HashMap;

use sqlparser::ast::{
    ArrayElemTypeDef, BinaryOperator, Cte, DataType, Distinct, Expr, Function, FunctionArg,
    FunctionArgExpr, FunctionArgumentList, FunctionArguments, GroupByExpr, Ident, JoinConstraint,
    JoinOperator, ObjectName, ObjectNamePart, OrderByKind, Query, SelectItem, SetExpr, Statement,
    TableAlias, TableFactor, UnaryOperator, Visit, VisitMut, Visitor, VisitorMut,
};

use crate::error::{unsupported, FrontendError, Result};

/// Refuse, anywhere in `statements`, the precedence shapes listed in the module documentation.
pub fn fix_precedence(statements: &mut [Statement]) -> Result<()> {
    for st in statements.iter() {
        if let ControlFlow::Break(e) = st.visit(&mut PrecedenceGuard) {
            return Err(e);
        }
    }
    Ok(())
}

struct PrecedenceGuard;

const CHAINED_IS: &str = "one IS operator directly over another (PostgreSQL's IS family is \
                          non-associative, so this does not parse there)";

impl Visitor for PrecedenceGuard {
    type Break = FrontendError;

    fn pre_visit_expr(&mut self, e: &Expr) -> ControlFlow<Self::Break> {
        let reason = match e {
            Expr::IsDistinctFrom(l, r) | Expr::IsNotDistinctFrom(l, r) => {
                if matches!(**r, Expr::BinaryOp { op: BinaryOperator::And | BinaryOperator::Or, .. })
                {
                    Some("IS [NOT] DISTINCT FROM over a bare AND/OR (the sqlparser 0.62 mis-parse)")
                } else if is_is_operator(l) || is_is_operator(r) {
                    Some(CHAINED_IS)
                } else if matches!(**r, Expr::UnaryOp { op: UnaryOperator::Not, .. }) {
                    Some("IS [NOT] DISTINCT FROM NOT ... (this shape is not rewritten)")
                } else {
                    None
                }
            }
            Expr::IsNull(x)
            | Expr::IsNotNull(x)
            | Expr::IsTrue(x)
            | Expr::IsNotTrue(x)
            | Expr::IsFalse(x)
            | Expr::IsNotFalse(x)
            | Expr::IsUnknown(x)
            | Expr::IsNotUnknown(x) => is_is_operator(x).then_some(CHAINED_IS),
            _ => None,
        };
        match reason {
            Some(r) => ControlFlow::Break(unsupported(r)),
            None => ControlFlow::Continue(()),
        }
    }
}

/// Whether `e` is, unparenthesised, one of the operators PostgreSQL parses at `IS` precedence.
fn is_is_operator(e: &Expr) -> bool {
    matches!(
        e,
        Expr::IsNull(_)
            | Expr::IsNotNull(_)
            | Expr::IsTrue(_)
            | Expr::IsNotTrue(_)
            | Expr::IsFalse(_)
            | Expr::IsNotFalse(_)
            | Expr::IsUnknown(_)
            | Expr::IsNotUnknown(_)
            | Expr::IsDistinctFrom(..)
            | Expr::IsNotDistinctFrom(..)
    )
}

/// A throwaway value to leave behind while a node is being moved out. Every one written is
/// overwritten before the function returns.
fn placeholder() -> Expr {
    Expr::Identifier(Ident::new(""))
}

/// Rewrite the constructs the prover has no meaning for into uninterpreted function calls.
///
/// ```text
/// payload ->> 'id'      becomes   q_str_jsonx(payload, 'id')
/// tags @> ARRAY['x']    becomes   q_bool_contains(tags, ARRAY['x'])
/// name !~ '^a'          becomes   NOT q_bool_rematch(name, '^a')
/// ts AT TIME ZONE 'utc' becomes   q_op_attz(ts, 'utc')
/// now()                 becomes   q_op_now(0)
/// ```
///
/// Three kinds of thing end up here, and they arrive by three different routes rather than because
/// they have anything in common: infix operators ([`demoted`]), a keyword-spelled binary function
/// (`AT TIME ZONE`), and the nullary clocks ([`CLOCKS`]). What they share is the treatment.
///
/// The prover's IR has no JSON type and no regex engine, so these operators cannot be modelled
/// concretely at all. What can be modelled is *less* than the operator: a function symbol with no
/// axioms beyond `equal arguments give equal results`. That is the classic abstraction, and it is
/// sound in one direction only — which happens to be the direction that matters. If the two IRs are
/// equivalent under **every** interpretation of the new symbol, they are equivalent under the real
/// one too, so a proof still transfers to the original pair. What is lost is proofs that need the
/// operator's actual behaviour, and a lost proof is a refusal rather than a wrong answer.
///
/// The name is what carries the return type downstream: [`infer`][crate::infer] reads the `q_int_` /
/// `q_str_` / `q_bool_` / `q_op_` prefix off it and [`casts`][crate::casts] synthesizes the matching
/// declaration. So this rewrite is also the only place that decides what type `->>` has, and the
/// answer is Postgres's: `text`.
///
/// # Preconditions
///
/// **The construct has to be a function of its arguments.** Every operator below is: they read their
/// operands and nothing else — no clock, no sequence, no session state. The one group here that does
/// read outside its arguments is [`CLOCKS`], which carries the extra argument this precondition would
/// otherwise deny it: a statement-stable clock has no arguments to be a function *of*, so what makes
/// it demotable is that it holds one value for the whole execution. The per-call-varying ones have no
/// such argument available and are refused outright in [`lower`][crate::lower] rather than demoted.
///
/// **The two sides of the pair have to get the same symbol for the same operator.** They do — the
/// mapping is a constant table keyed on the operator, not on anything about the query. Were it
/// query-dependent, two spellings of the same operator would become two unrelated symbols and the
/// abstraction would stop being conservative in the safe direction.
///
/// # Two places where the mapping says more than the minimum
///
/// **`<@` is `@>` with its arguments swapped.** They are the same containment relation read in the two
/// directions, so giving them one symbol lets the prover see `a @> b` and `b <@ a` as the same thing
/// rather than as two opaque calls. Likewise `!~` becomes a negated `~` instead of a symbol of its
/// own: in Postgres `x !~ p` *is* `NOT (x ~ p)`, nulls included, since both sides are `NULL` when
/// either operand is and `NOT NULL` is `NULL`.
///
/// **`|`, `#`, `<<` and the rest of the bitwise family are left refused.** `|` is bitwise-or on
/// integers, union on `tsquery`, and something else again on `inet`; a single symbol for it would be
/// fine, but the return type the prefix has to commit to would be a guess, and there are 3 such rows.
pub fn demote_operators(statements: &mut [Statement]) {
    let mut demote = Demote;
    for st in statements {
        let _ = st.visit(&mut demote);
    }
}

/// The uninterpreted call an operator becomes: its name, whether the operands are swapped, and
/// whether the result is negated. `None` for every operator lowered natively or left refused.
fn demoted(op: &BinaryOperator) -> Option<(&'static str, bool, bool)> {
    use BinaryOperator::*;
    Some(match op {
        // `->` and `#>` yield json/jsonb, which has no counterpart in the prover's five types, so
        // the opaque sort it is; `->>` and `#>>` yield text.
        Arrow | HashArrow => ("q_op_jsonx", false, false),
        LongArrow | HashLongArrow => ("q_str_jsonx", false, false),
        // Containment, in both directions off one symbol.
        AtArrow => ("q_bool_contains", false, false),
        ArrowAt => ("q_bool_contains", true, false),
        // `@@` is the text-search match and the jsonpath predicate check; both are boolean.
        AtAt => ("q_bool_atat", false, false),
        // `&&` is overlap, on arrays and on ranges alike.
        PGOverlap => ("q_bool_overlap", false, false),
        PGRegexMatch => ("q_bool_rematch", false, false),
        PGRegexNotMatch => ("q_bool_rematch", false, true),
        PGRegexIMatch => ("q_bool_reimatch", false, false),
        PGRegexNotIMatch => ("q_bool_reimatch", false, true),
        _ => return None,
    })
}

/// The clocks that hold still for the duration of one query, and the symbol each becomes.
///
/// The precondition is stability *within a single execution*, not across statements: the prover asks
/// whether two queries agree for every database state, the clock is part of that state, and a
/// constant symbol shared by both sides asks exactly that. `clock_timestamp()` and `random()` fail the
/// test — they move during one query — and stay refused by [`lower`][crate::lower]'s
/// `NONDETERMINISTIC`, which is why they are absent here.
///
/// Spellings share a symbol only where Postgres makes them the same value. `now()`,
/// `current_timestamp` and `transaction_timestamp()` are one value under three names and are unified;
/// everything else gets its own. That is not fussiness: the preprocessor folds `current_date` into the
/// same `q_int_now` as `current_timestamp`, which asserts that a date equals a timestamp, so a pair
/// rewriting one into the other would come out provably equivalent. A distinct symbol can only ever
/// cost a proof; a shared one can manufacture one.
///
/// The type is opaque rather than the preprocessor's `q_int_`. Calling a timestamp an integer hands
/// the prover integer arithmetic and a total order over it — the assumption [`lower`][crate::lower]'s
/// `UNDECLARED_RET` documents at length and declines.
const CLOCKS: [(&str, &str); 7] = [
    ("NOW", "q_op_now"),
    ("CURRENT_TIMESTAMP", "q_op_now"),
    ("TRANSACTION_TIMESTAMP", "q_op_now"),
    ("STATEMENT_TIMESTAMP", "q_op_stmt_ts"),
    ("LOCALTIMESTAMP", "q_op_localts"),
    ("CURRENT_DATE", "q_op_curdate"),
    ("CURRENT_TIME", "q_op_curtime"),
];

/// The JSON extraction *functions*, and the symbol each becomes.
///
/// These are the call spellings of the extraction operators in [`demoted`]: `json_extract_path_text(p,
/// 'a')` and `p #>> 'a'` are one Postgres operation written two ways. Unifying them is the same move
/// that gives `@>` and `<@` one symbol, and it is what the abstraction requires — a pair that rewrites
/// between the two spellings otherwise gets two unrelated symbols and can never be proved, though the
/// rewrite is exactly the kind an optimizer performs.
///
/// The return type follows Postgres, so the split is the same one the operators make: the `_text` and
/// `_scalar` forms yield `text` and take `q_str_`, the rest yield `json`/`jsonb` and take `q_op_`.
///
/// # Why all eight, when the operator spellings are the ones that occur
///
/// Six of these are the names a dialect-aware SQL library will already bucket as JSON extraction;
/// `jsonb_extract_path` and `jsonb_extract_path_text` are routinely missed, falling through to a
/// generic "anonymous function" node instead. That omission is an artifact of a dialect table rather
/// than a decision — they are the exact `jsonb` counterparts of two names that do get bucketed — so
/// all eight are listed here.
///
/// Reach was measured, not assumed: the call spellings are rare next to `->` and `->>`, which is what
/// real queries overwhelmingly write. So this closes a parity gap and an asymmetry between spellings;
/// it is not expected to move a verdict on its own.
const JSON_FNS: [(&str, &str); 8] = [
    ("JSON_EXTRACT", "q_op_jsonx"),
    ("JSONB_EXTRACT", "q_op_jsonx"),
    ("JSON_EXTRACT_PATH", "q_op_jsonx"),
    ("JSONB_EXTRACT_PATH", "q_op_jsonx"),
    ("JSON_EXTRACT_SCALAR", "q_str_jsonx"),
    ("JSONB_EXTRACT_SCALAR", "q_str_jsonx"),
    ("JSON_EXTRACT_PATH_TEXT", "q_str_jsonx"),
    ("JSONB_EXTRACT_PATH_TEXT", "q_str_jsonx"),
];

/// The symbol a JSON extraction call becomes, if `f` is one.
///
/// Matched on the bare, unqualified name, as [`clock_symbol`] is and for the same reason: a qualified
/// `myschema.json_extract` is somebody's own function and means nothing to us. Every modifier a call
/// can carry disqualifies it too — `OVER`, `FILTER`, `DISTINCT`, an `ORDER BY`, a named or wildcard
/// argument. None of them is meaningful on these functions, so one appearing means the call is not
/// what the name suggests, and leaving it alone costs a proof where rewriting it would risk a wrong
/// one.
fn json_fn_symbol(f: &Function) -> Option<&'static str> {
    let [part] = &f.name.0[..] else { return None };
    let name = part.as_ident()?.value.to_uppercase();
    if f.over.is_some() || f.filter.is_some() || f.null_treatment.is_some() {
        return None;
    }
    let FunctionArguments::List(l) = &f.args else { return None };
    // At least two: these all read a document and one or more path elements, and the path is variadic
    // (`json_extract_path(p, 'a', 'b')`), so the arity is preserved rather than fixed at two.
    if l.args.len() < 2 || !l.clauses.is_empty() || l.duplicate_treatment.is_some() {
        return None;
    }
    if !l.args.iter().all(|a| matches!(a, FunctionArg::Unnamed(FunctionArgExpr::Expr(_)))) {
        return None;
    }
    JSON_FNS.iter().find(|(n, _)| *n == name).map(|(_, s)| *s)
}

/// Move the positional arguments out of a call, leaving placeholders behind.
///
/// Only ever called after [`json_fn_symbol`] has established that every argument is positional, so
/// the filter drops nothing.
fn take_positional_args(f: &mut Function) -> Vec<Expr> {
    let FunctionArguments::List(l) = &mut f.args else { return Vec::new() };
    l.args
        .iter_mut()
        .filter_map(|a| match a {
            FunctionArg::Unnamed(FunctionArgExpr::Expr(x)) => {
                Some(core::mem::replace(x, placeholder()))
            }
            _ => None,
        })
        .collect()
}

/// The symbol a nullary clock call becomes, if `f` is one.
///
/// Matched on the bare, unqualified name with no arguments: `now(x)` is somebody's own function, and
/// a qualified `myschema.now()` may be too.
fn clock_symbol(f: &Function) -> Option<&'static str> {
    let [part] = &f.name.0[..] else { return None };
    let name = part.as_ident()?.value.to_uppercase();
    // Both spellings reach here: `CURRENT_TIMESTAMP` parses with no argument list at all, `now()`
    // with an empty one. Anything carrying real arguments, a `FILTER` or an `OVER` is not the
    // constant.
    let nullary = match &f.args {
        FunctionArguments::None => true,
        FunctionArguments::List(l) => l.args.is_empty() && l.clauses.is_empty(),
        FunctionArguments::Subquery(_) => false,
    };
    if !nullary || f.over.is_some() || f.filter.is_some() {
        return None;
    }
    CLOCKS.iter().find(|(n, _)| *n == name).map(|(_, s)| *s)
}

struct Demote;

impl VisitorMut for Demote {
    type Break = ();

    /// Post-order, so an operator nested in another operator's operand is already a call by the time
    /// the outer one is rewritten.
    fn post_visit_expr(&mut self, e: &mut Expr) -> ControlFlow<Self::Break> {
        // `x AT TIME ZONE z` is an ordinary deterministic function of both operands; the only reason
        // it is not already a call is that the grammar gives it a keyword spelling.
        if let Expr::AtTimeZone { timestamp, time_zone } = e {
            let a = core::mem::replace(&mut **timestamp, placeholder());
            let b = core::mem::replace(&mut **time_zone, placeholder());
            *e = call2("q_op_attz", a, b);
            return ControlFlow::Continue(());
        }
        // A clock is nullary and the prover's declaration DSL has no zero-argument form, so it becomes
        // a call on a filler literal. Collapsing every occurrence onto one constant is the point
        // rather than a cost: within one query these really are one value.
        if let Expr::Function(f) = e {
            if let Some(sym) = clock_symbol(f) {
                *e = call1(sym, Expr::Value(number_zero()));
                return ControlFlow::Continue(());
            }
            // The call spelling of an extraction operator, onto the same symbol the operator gets.
            // Arity is carried through: the path argument is variadic.
            if let Some(sym) = json_fn_symbol(f) {
                let args = take_positional_args(f);
                *e = calln(sym, args);
                return ControlFlow::Continue(());
            }
        }
        let Expr::BinaryOp { left, op, right } = e else {
            return ControlFlow::Continue(());
        };
        let Some((name, swap, negate)) = demoted(op) else {
            return ControlFlow::Continue(());
        };
        let mut a = core::mem::replace(&mut **left, placeholder());
        let mut b = core::mem::replace(&mut **right, placeholder());
        if swap {
            core::mem::swap(&mut a, &mut b);
        }
        let call = call2(name, a, b);
        *e = if negate {
            Expr::UnaryOp { op: UnaryOperator::Not, expr: Box::new(call) }
        } else {
            call
        };
        ControlFlow::Continue(())
    }
}

/// `name(a, b)`, in the shape sqlparser would have parsed such a call into.
fn call2(name: &str, a: Expr, b: Expr) -> Expr {
    calln(name, vec![a, b])
}

/// `name(a)` — the shape a nullary construct takes once it has been given its filler argument.
fn call1(name: &str, a: Expr) -> Expr {
    calln(name, vec![a])
}

/// The integer `0`, which is only ever a placeholder: it stands in the argument list of a symbol that
/// really takes none, because the prover's declaration DSL cannot spell a zero-argument function.
fn number_zero() -> sqlparser::ast::ValueWithSpan {
    sqlparser::ast::Value::Number("0".into(), false).into()
}

fn calln(name: &str, args: Vec<Expr>) -> Expr {
    Expr::Function(Function {
        name: ObjectName(vec![ObjectNamePart::Identifier(Ident::new(name))]),
        uses_odbc_syntax: false,
        parameters: FunctionArguments::None,
        args: FunctionArguments::List(FunctionArgumentList {
            duplicate_treatment: None,
            args: args
                .into_iter()
                .map(|x| FunctionArg::Unnamed(FunctionArgExpr::Expr(x)))
                .collect(),
            clauses: Vec::new(),
        }),
        filter: None,
        null_treatment: None,
        over: None,
        within_group: Vec::new(),
    })
}

/// Drop the `DISTINCT` from the top `SELECT` of an `IN` or `EXISTS` subquery.
///
/// `x IN (SELECT DISTINCT e …)` ≡ `x IN (SELECT e …)`, and likewise for `EXISTS`: membership and
/// existence are questions about a *set*, so the duplicates the `DISTINCT` removes were never
/// visible to the answer. Removing it shrinks the relation the prover has to reason about, which is
/// the point — a `DISTINCT` is a grouping in the IR, and one fewer grouping is one fewer thing to
/// discharge.
///
/// Only that position. A top-level `DISTINCT`, or one on a derived table, is observable and is left
/// alone.
///
/// # Preconditions
///
/// Two, and each has a counterexample rather than a caveat.
///
/// **No row slice on the subquery.** `LIMIT`/`OFFSET`/`FETCH` is applied *after* the `DISTINCT`, so
/// it does see the multiplicity that was removed. Over `t = {1, 1, 2}`:
///
/// ```text
/// IN (SELECT DISTINCT e FROM t ORDER BY e LIMIT 2)   -- {1, 2}
/// IN (SELECT e FROM t ORDER BY e LIMIT 2)            -- {1}
/// ```
///
/// **Not `DISTINCT ON`.** It is spelled like a modifier of `DISTINCT` and is a different operator:
/// it keeps one row per key and drops the rest, so it removes *values*, not copies of them. Over
/// `t = {(1,10), (1,20)}`, `20 IN (SELECT DISTINCT ON (a) b FROM t ORDER BY a, b)` is false while
/// `20 IN (SELECT b FROM t)` is true.
///
/// Both guards are uniform over `IN` and `EXISTS`, though under `EXISTS` both are provably harmless
/// — non-emptiness survives de-duplication, and `DISTINCT ON` of a non-empty relation is non-empty.
/// A bare `OFFSET` under `EXISTS` is *not* harmless, so the carve-out would have to be partial, and
/// half an exemption is worth less than a rule that reads in one line.
///
/// Both guards are also per-subquery: one blocked subquery does not freeze its siblings. That is
/// safe in a way the `ORDER BY` strip is not — a slice nested deeper runs *before* anything at this
/// level, so it cannot see the multiplicity removed here.
pub fn strip_in_exists_distinct(statements: &mut [Statement]) {
    let mut strip = Strip;
    for st in statements {
        let _ = st.visit(&mut strip);
    }
}

struct Strip;

impl VisitorMut for Strip {
    type Break = ();

    fn post_visit_expr(&mut self, e: &mut Expr) -> ControlFlow<Self::Break> {
        let subquery = match e {
            Expr::InSubquery { subquery, .. } | Expr::Exists { subquery, .. } => &mut **subquery,
            _ => return ControlFlow::Continue(()),
        };
        // The slice lives on the `Query`, not on the `SELECT`, and `FETCH FIRST n ROWS ONLY` is a
        // third field rather than a spelling of `LIMIT` — so all three have to be checked.
        if subquery.limit_clause.is_some() || subquery.fetch.is_some() {
            return ControlFlow::Continue(());
        }
        if let SetExpr::Select(select) = &mut *subquery.body {
            // `Distinct::On` is the one that drops rows; `Distinct::All` is an explicit `ALL`, which
            // is not a `DISTINCT` to remove in the first place.
            if matches!(select.distinct, Some(Distinct::Distinct)) {
                select.distinct = None;
            }
        }
        ControlFlow::Continue(())
    }
}

/// `x IN (SELECT unnest(A))` -> `x = ANY(A)`, in filter position only.
///
/// The two are the same optimization written two ways, and it turns up in the wild as exactly that
/// pair: one side spells `col = ANY($1)`, the other `col IN (SELECT unnest($1))`. The first lowers
/// today ([`lower_quantified`][crate::lower]'s uninterpreted arm); the second refuses, because
/// `unnest` is set-returning and lowering it as a scalar would understate the cardinality. Neither
/// side can be proved against a refusal, so every such pair was undecided for a syntactic reason.
///
/// # Why this needs a polarity guard
///
/// After the rewrite both spellings lower to the *same* uninterpreted boolean symbol `= ANY`. That is
/// an assertion that they denote the same function, and strictly they do not:
///
/// ```text
/// x = ANY(NULL::int[])                  -- NULL
/// x IN (SELECT unnest(NULL::int[]))     -- FALSE: unnest of NULL yields zero rows
/// ```
///
/// (Only a NULL *array* diverges. NULL array *elements* agree — both give NULL or TRUE — and a
/// multidimensional array agrees too, since `unnest` flattens and `= ANY` searches all elements.)
///
/// NULL and FALSE are indistinguishable wherever the only question asked is "is this TRUE", which is
/// exactly what a filter asks. So the rewrite is confined to `WHERE`, `HAVING` and join `ON`, and
/// within one of those it descends only through `AND`, `OR` and parentheses. The claim, for `E` built
/// from a hole using only those three:
///
/// > `E[NULL]` is TRUE iff `E[FALSE]` is TRUE.
///
/// Induction on `E`. For the bare hole, neither NULL nor FALSE is TRUE. `A AND B` is TRUE iff both
/// operands are and `A OR B` iff either is; the operand not containing the hole is unchanged, so the
/// hypothesis carries. A join `ON` counts as a filter even on an outer join: a row whose `ON` is not
/// TRUE is null-extended, and NULL and FALSE are both not TRUE.
///
/// Every excluded context is excluded because it breaks that step. `NOT FALSE` is TRUE while
/// `NOT NULL` is not; `FALSE IS NULL` is FALSE while `NULL IS NULL` is TRUE; a projection, a
/// `CASE` result and a boolean aggregate all report the value itself rather than testing it.
///
/// A `CASE` *condition* would in fact be safe by the same argument -- NULL and FALSE both take the
/// `ELSE` branch -- and is left out because nothing needs it. That is the one place it would matter:
/// [`dml::reduce`][crate::dml] turns an `UPDATE`'s `WHERE` into a `CASE` condition, so this pass sees
/// no `UPDATE` predicates. No corpus row is affected, and a missed rewrite costs a refusal.
///
/// Runs after [`strip_in_exists_distinct`], which will already have removed a bare `DISTINCT` from
/// the subquery. `Distinct::Distinct` is accepted here anyway so the two passes are order-independent
/// -- de-duplicating a bag before `IN` is invisible, and the only case the earlier pass declines is a
/// subquery with a row slice, which this one rejects outright.
pub fn unnest_in_to_any(statements: &mut [Statement]) {
    let mut v = UnnestInToAny;
    for st in statements {
        let _ = st.visit(&mut v);
    }
}

struct UnnestInToAny;

impl VisitorMut for UnnestInToAny {
    type Break = ();

    /// The visitor is used only to *reach* every [`Query`] in the tree, including those nested in
    /// expressions. The descent into filter positions is hand-written below, because the derived
    /// walk visits every expression regardless of the polarity it sits in.
    fn pre_visit_query(&mut self, q: &mut Query) -> ControlFlow<Self::Break> {
        rewrite_filters(&mut q.body);
        ControlFlow::Continue(())
    }
}

/// Apply the rewrite to every filter this set expression owns directly.
fn rewrite_filters(body: &mut SetExpr) {
    match body {
        SetExpr::Select(select) => {
            for e in [select.selection.as_mut(), select.having.as_mut()].into_iter().flatten() {
                rewrite_in_filter(e);
            }
            for join in select.from.iter_mut().flat_map(|t| t.joins.iter_mut()) {
                if let Some(e) = join_on_mut(&mut join.join_operator) {
                    rewrite_in_filter(e);
                }
            }
        }
        SetExpr::SetOperation { left, right, .. } => {
            rewrite_filters(left);
            rewrite_filters(right);
        }
        // `SetExpr::Query` is a `Query` and is reached by `pre_visit_query` in its own right, so
        // descending here would process it twice. Everything else owns no filter of its own.
        _ => {}
    }
}

/// The `ON` expression of a join, for the operators [`lower`][crate::lower] actually lowers.
///
/// Deliberately the same list as `join_op` there: a rewrite inside an operator that is refused later
/// is dead code, and if that list widens, this one being narrow costs a refusal and not a proof.
fn join_on_mut(op: &mut JoinOperator) -> Option<&mut Expr> {
    use JoinOperator::*;
    let c = match op {
        Join(c) | Inner(c) | Left(c) | LeftOuter(c) | Right(c) | RightOuter(c) | FullOuter(c)
        | CrossJoin(c) => c,
        _ => return None,
    };
    match c {
        JoinConstraint::On(e) => Some(e),
        _ => None,
    }
}

/// Descend a filter through the connectives that preserve "is this TRUE", rewriting what matches.
fn rewrite_in_filter(e: &mut Expr) {
    match e {
        Expr::BinaryOp { op: BinaryOperator::And | BinaryOperator::Or, left, right } => {
            rewrite_in_filter(left);
            rewrite_in_filter(right);
        }
        Expr::Nested(inner) => rewrite_in_filter(inner),
        Expr::InSubquery { expr, subquery, negated: false } => {
            if let Some(arg) = sole_unnest_arg(subquery) {
                *e = Expr::AnyOp {
                    left: expr.clone(),
                    compare_op: BinaryOperator::Eq,
                    right: Box::new(arg.clone()),
                    is_some: false,
                };
            }
        }
        // Anything else either breaks the induction above or cannot contain a filter.
        _ => {}
    }
}

/// The argument of `X` in a subquery that is *exactly* `SELECT [DISTINCT] unnest(X)`.
///
/// Every clause is checked rather than the few that look dangerous: the rewrite discards the whole
/// query and keeps only this one expression, so any clause left unexamined is a clause silently
/// dropped. A `FROM` is the clearest case -- `IN (SELECT unnest(a) FROM t)` unnests once per row of
/// `t` -- but `WHERE`, `LIMIT`, `GROUP BY` and a set operation are each just as observable.
fn sole_unnest_arg(q: &Query) -> Option<&Expr> {
    if q.with.is_some()
        || q.order_by.is_some()
        || q.limit_clause.is_some()
        || q.fetch.is_some()
        || q.for_clause.is_some()
        || q.settings.is_some()
        || q.format_clause.is_some()
        || !q.locks.is_empty()
        || !q.pipe_operators.is_empty()
    {
        return None;
    }
    let SetExpr::Select(s) = &*q.body else { return None };
    // `Distinct::On` drops rows and `Distinct::All` is not a `DISTINCT`; see `strip_in_exists_distinct`.
    if !matches!(s.distinct, None | Some(Distinct::Distinct)) {
        return None;
    }
    if s.top.is_some()
        || s.into.is_some()
        || s.prewhere.is_some()
        || s.selection.is_some()
        || s.having.is_some()
        || s.qualify.is_some()
        || s.exclude.is_some()
        || s.select_modifiers.is_some()
        || s.value_table_mode.is_some()
        || !s.from.is_empty()
        || !s.lateral_views.is_empty()
        || !s.connect_by.is_empty()
        || !s.cluster_by.is_empty()
        || !s.distribute_by.is_empty()
        || !s.sort_by.is_empty()
        || !s.named_window.is_empty()
        || !s.optimizer_hints.is_empty()
    {
        return None;
    }
    match &s.group_by {
        GroupByExpr::Expressions(es, mods) if es.is_empty() && mods.is_empty() => {}
        _ => return None,
    }
    let [item] = s.projection.as_slice() else { return None };
    // An alias is not observable through `IN`, which reads the bag and not the column name.
    let (SelectItem::UnnamedExpr(proj) | SelectItem::ExprWithAlias { expr: proj, .. }) = item
    else {
        return None;
    };
    let Expr::Function(f) = proj else { return None };
    // The *bare* name, as `lower`'s `SET_RETURNING` check does it: `pg_catalog.unnest(x)` is still
    // unnest, and the refusal this replaces was keyed the same way.
    if !f.name.0.last()?.as_ident()?.value.eq_ignore_ascii_case("unnest") {
        return None;
    }
    if f.uses_odbc_syntax
        || f.filter.is_some()
        || f.over.is_some()
        || f.null_treatment.is_some()
        || !f.within_group.is_empty()
        || !matches!(f.parameters, FunctionArguments::None)
    {
        return None;
    }
    let FunctionArguments::List(list) = &f.args else { return None };
    if list.duplicate_treatment.is_some() || !list.clauses.is_empty() {
        return None;
    }
    // Exactly one positional argument. `unnest(a, b)` is the multi-array form, which produces a row
    // per position across both arrays and has no `= ANY` equivalent.
    let [FunctionArg::Unnamed(FunctionArgExpr::Expr(arg))] = list.args.as_slice() else {
        return None;
    };
    Some(arg)
}

/// Does this query carry a row slice of its own?
fn has_row_slice(q: &Query) -> bool {
    q.limit_clause.is_some() || q.fetch.is_some()
}

/// Whether a query's own body applies `DISTINCT ON`, which — like a row slice — makes the enclosing
/// `ORDER BY` observable rather than dead.
///
/// Recurses through set operations because a `DISTINCT ON` there is still governed by this query's
/// clause. A nested [`Query`] is *not* recursed into: it carries its own `ORDER BY` and the walkers
/// here visit it in its own right.
fn body_has_distinct_on(body: &SetExpr) -> bool {
    match body {
        SetExpr::Select(s) => matches!(s.distinct, Some(Distinct::On(_))),
        SetExpr::SetOperation { left, right, .. } => {
            body_has_distinct_on(left) || body_has_distinct_on(right)
        }
        _ => false,
    }
}

/// Push a cast over an array *literal* down onto the literal's elements.
///
/// ```text
/// ARRAY[e1, …, en]::T[]   ->   ARRAY[e1::T, …, en::T]
/// ```
///
/// ## Why this is an equivalence
///
/// A Postgres array cast is defined element-wise: `ARRAY[a,b]::bigint[]` casts `a` and `b` to
/// `bigint` and builds a `bigint[]` from the results, which is what the right-hand side spells
/// out. Array-ness is preserved (the target *is* an array type — see the guard), the element
/// order and count are untouched, and a cast that would error or yield NULL on an element does
/// the same thing on either side.
///
/// ## Why it is worth doing
///
/// [`casts`][crate::casts] refuses a cast over an `Expr::Array` on purpose, and correctly:
/// `lower_expr` has no arm for a cast whose operand is an array, so wrapping it in a `qcast`
/// would move the refusal rather than lift it, *and* it would cost the exact OR-expansion
/// [`lower_quantified`][crate::lower] gives a bare array literal under `= ANY` / `<> ALL`. The
/// fix is not to weaken that refusal but to make the shape not arise: after this rewrite there
/// is no cast over an array anywhere, and each element cast is an ordinary scalar cast that
/// `casts`'s rules already handle — `$1::bigint` hits rule 1 and hoists, so inference types the
/// placeholder from the element type.
///
/// This runs **before** [`crate::infer`] for that last reason, and after [`unnest_in_to_any`],
/// which is what turns `IN (SELECT unnest(ARRAY[$1]::t[]))` into `= ANY(ARRAY[$1]::t[])` — that
/// is how several of the rows this reaches acquire the shape at all.
///
/// ## Two guards
///
/// **The target must be an array type.** `ARRAY[a,b]::text` is not an element-wise cast — it is a
/// cast of the whole array to a scalar, and distributing it would assert something the query does
/// not say. Only `T[]`, `T[k]` and the `T ARRAY` / `ARRAY<T>` spellings qualify, and
/// [`ArrayElemTypeDef::None`] (a bare `ARRAY` with no element type) does not: there is no `T` to
/// push down.
///
/// **The literal must be non-empty.** `ARRAY[]::T[]` has nowhere to put `T`, so distributing it
/// would silently discard the only statement of the element type. It keeps its refusal, which
/// costs nothing measurable — the empty-array occurrences are all in `COALESCE` position, where
/// lowering cannot use them either way.
///
/// Nesting needs no special case. The visitor is post-order, so `ARRAY[ARRAY[a]::t[]]::t[][]`
/// has its inner cast distributed first; the outer then distributes `t[]` onto an element that is
/// already an `Expr::Array`, producing a cast over an array one level down that the next
/// post-order visit does not revisit. That residue is a cast over an array literal, so it stays
/// refused — half-rewritten is not reachable, because the refusal is on the shape and not on this
/// pass having touched it.
pub fn distribute_array_casts(statements: &mut [Statement]) {
    let mut d = DistributeArrayCast;
    for st in statements {
        let _ = st.visit(&mut d);
    }
}

struct DistributeArrayCast;

impl VisitorMut for DistributeArrayCast {
    type Break = ();

    /// Post-order, so an array cast nested inside another one is distributed from the inside out.
    fn post_visit_expr(&mut self, e: &mut Expr) -> ControlFlow<Self::Break> {
        let Expr::Cast { expr, data_type, kind, format } = e else {
            return ControlFlow::Continue(());
        };
        // `CAST(x AS t FORMAT f)` is not Postgres, and its meaning over an array would be a guess.
        // The `T ARRAY` spelling is declined by `array_elem_type` itself.
        if format.is_some() {
            return ControlFlow::Continue(());
        }
        let Some(elem) = array_elem_type(data_type) else {
            return ControlFlow::Continue(());
        };
        let Expr::Array(arr) = &mut **expr else {
            return ControlFlow::Continue(());
        };
        if arr.elem.is_empty() {
            return ControlFlow::Continue(());
        }
        let kind = kind.clone();
        let items = core::mem::take(&mut arr.elem);
        *e = Expr::Array(sqlparser::ast::Array {
            elem: items
                .into_iter()
                .map(|it| Expr::Cast {
                    kind: kind.clone(),
                    expr: Box::new(it),
                    data_type: elem.clone(),
                    format: None,
                })
                .collect(),
            named: arr.named,
        });
        ControlFlow::Continue(())
    }
}

/// The element type of an array target, or `None` if the target is not an array type — or is an
/// array with no element type to push down.
///
/// `SquareBracket`'s size is dropped deliberately: `T[3]` constrains the array, not the element, so
/// the element cast is `::T` either way. Postgres does not enforce declared array sizes at all.
pub(crate) fn array_elem_type(dt: &DataType) -> Option<DataType> {
    match dt {
        DataType::Array(def) => match def {
            ArrayElemTypeDef::AngleBracket(t)
            | ArrayElemTypeDef::SquareBracket(t, _)
            | ArrayElemTypeDef::Parenthesis(t) => Some((**t).clone()),
            // A bare `ARRAY` with no element type: nothing to distribute.
            ArrayElemTypeDef::None => None,
            // The SQL-standard `T ARRAY` / `T ARRAY[n]`. Postgres reads it as `T[]`, but it is left
            // undistributed, as `CAST(x AS T ARRAY)` always was, rather than grow what can be proved
            // on a parser upgrade.
            ArrayElemTypeDef::Qualified(..) => None,
        },
        _ => None,
    }
}

/// Drop every `ORDER BY`, but only if nothing anywhere in the tree consumes an ordering.
///
/// The prover compares relations as bags, in which row order is not part of the value, so an
/// `ORDER BY` with nothing downstream to consume it is dead. Two constructs consume one, and either
/// one anywhere blocks the strip for the whole tree:
///
/// - a **row slice**, which is the opposite of dead — it is what *chooses* the rows the slice keeps;
/// - **`DISTINCT ON`**, which chooses the surviving row of each key group. It is the subtler of the
///   two because it leaves no `Sort` behind: `lower::distinct_on` encodes the ordering into
///   the name of an opaque operator instead, so stripping the clause here would make
///   `DISTINCT ON (k) … ORDER BY k, t` and `… ORDER BY k, t DESC` mint the *same* symbol and prove
///   equal. Any lowering that reads a clause this pass can delete has to be listed here.
///
/// Tree-wide, unlike [`strip_in_exists_distinct`]'s per-node guards, and deliberately so: this
/// clears orderings at every level at once, including ones above a slice, so "somewhere else" is
/// exactly the case that has to stop it.
///
/// Only query-level orderings. `ORDER BY` inside a window spec or an ordered-set aggregate is part
/// of that operator's value and lives on a different node.
pub fn strip_dead_order_by(queries: &mut [Query]) {
    let mut find = FindSlice(false);
    for q in queries.iter() {
        let _ = q.visit(&mut find);
    }
    if find.0 {
        return;
    }
    let mut strip = StripOrder;
    for q in queries {
        let _ = q.visit(&mut strip);
    }
}

struct FindSlice(bool);

impl Visitor for FindSlice {
    type Break = ();

    fn pre_visit_query(&mut self, q: &Query) -> ControlFlow<Self::Break> {
        if has_row_slice(q) || body_has_distinct_on(q.body.as_ref()) {
            self.0 = true;
            return ControlFlow::Break(());
        }
        ControlFlow::Continue(())
    }
}

struct StripOrder;

impl VisitorMut for StripOrder {
    type Break = ();

    fn pre_visit_query(&mut self, q: &mut Query) -> ControlFlow<Self::Break> {
        q.order_by = None;
        ControlFlow::Continue(())
    }
}

/// Strip a top-level `ORDER BY … LIMIT … OFFSET …` that is **identical on both sides** of the pair.
///
/// With the same pagination applied to both, the pair reduces to its inner queries: if those return
/// the same bag then the same ordering over the same bag yields the same page. The prover has no
/// `LIMIT` in its IR and the frontend refuses one, so without this the whole pair is unlowerable —
/// which is why this is worth doing rather than refusing.
///
/// # Why "identical" is load-bearing, and why it is not sufficient
///
/// An `ORDER BY` that does not totally order the bag leaves the page under-determined; the query
/// then denotes a *set* of legal answers rather than one answer. That is fine for this rewrite —
/// equal bags give the two sides the same set of legal answers — but only when the bag is enough to
/// determine that set. It is not, if the ordering reads something the projection threw away:
///
/// ```text
/// A: SELECT a FROM t ORDER BY b LIMIT 1     t = {(1,1), (2,2)}   -- yields 1
/// B: SELECT a FROM u ORDER BY b LIMIT 1     u = {(1,2), (2,1)}   -- yields 2
/// ```
///
/// `SELECT a FROM t` and `SELECT a FROM u` are both the bag `{1, 2}`, so the prover would prove the
/// stripped pair and the verdict would be reported for a pair that returns different rows. So every
/// `ORDER BY` key must be **determined by the projection**: an ordinal, an output column name, or an
/// expression the `SELECT` list already computes. Anything else, and the pagination stays on and the
/// pair is refused downstream.
///
/// Ties *within* the projected output are not a problem: both sides have the same output bag, so
/// they have the same set of legal pages, which is all equivalence can mean for a query that does
/// not pin one.
///
/// # The condition is asked of both sides
///
/// Identical clause text pins the `ORDER BY` *spelling*, not what that spelling resolves to, so the
/// condition is not symmetric and holding on one side says nothing about the other. Each side names
/// its own key against its own projection, and both readings have to be determined:
///
/// ```text
/// A: SELECT y FROM (SELECT a AS y FROM t) AS v        ORDER BY y LIMIT 1   -- yields min(a)
/// B: SELECT x FROM (SELECT a AS x, b AS y FROM t) AS v ORDER BY y LIMIT 1  -- yields a of min(b)
/// ```
///
/// Both read `ORDER BY y LIMIT 1`. Both return the bag of `t.a`, so the stripped pair is provable.
/// But A's `y` *is* its output while B's `y` is `t.b`, which B does not project — so over
/// `t = {(a=1, b=2), (a=2, b=1)}` A yields `1` and B yields `2`. Checking A alone would strip and
/// report a verdict for a pair whose two sides return different rows.
pub fn strip_identical_pagination(queries: &mut [Query]) {
    let [a, b] = queries else { return };
    if !(has_row_slice(a) || has_row_slice(b)) {
        return;
    }
    // A top-level `DISTINCT ON` reads this `ORDER BY` to pick its rows, so it is not just the page's
    // ordering and cannot be dropped along with the page. See [`strip_dead_order_by`].
    if body_has_distinct_on(a.body.as_ref()) || body_has_distinct_on(b.body.as_ref()) {
        return;
    }
    let clauses = |q: &Query| {
        (
            q.order_by.as_ref().map(ToString::to_string),
            q.limit_clause.as_ref().map(ToString::to_string),
            q.fetch.as_ref().map(ToString::to_string),
        )
    };
    if clauses(a) != clauses(b) {
        return;
    }
    // Both sides: each names its key against its own projection, and identical clause text does not
    // make one reading stand in for the other.
    if !order_determined_by_projection(a) || !order_determined_by_projection(b) {
        return;
    }
    for q in [a, b] {
        q.order_by = None;
        q.limit_clause = None;
        q.fetch = None;
    }
}

/// Drop the row-locking clauses (`FOR UPDATE`, `FOR SHARE`, with `SKIP LOCKED` or `NOWAIT`) when both
/// sides carry exactly the same ones in the same places; otherwise leave every one in place, for
/// lowering to refuse.
///
/// The prover evaluates one query against one database state, with no other transaction running. In
/// that model no row is ever locked by anyone else, so `FOR UPDATE SKIP LOCKED` returns what the bare
/// query returns and `NOWAIT` never fails: every lock clause is inert, which is why lowering used to
/// ignore them. Under concurrency they are not inert: `SKIP LOCKED` changes which rows come back and
/// `NOWAIT` turns a wait into an error. So a lock clause on one side only, or two different ones, is a
/// difference the prover cannot see, and ignoring it would prove the pair.
///
/// "The same places" is the pre-order position of the query node that carries each clause, which is
/// what survives [`inline_ctes`]: the usual queue shape, `WITH c AS (SELECT .. FOR UPDATE SKIP LOCKED)
/// UPDATE ..`, has its clause on a derived table by the time this runs. Taking identical clauses in
/// identical positions to lock the same way is the same kind of assumption
/// [`strip_identical_pagination`] makes about a shared page.
pub fn strip_identical_locks(queries: &mut [Query]) {
    let [a, b] = queries else { return };
    let sites = lock_sites(a);
    if sites.is_empty() || sites != lock_sites(b) {
        return;
    }
    for q in [a, b] {
        let _ = q.visit(&mut ClearLocks);
    }
}

/// Every lock clause in `q`, with the pre-order position of the query node that carries it.
fn lock_sites(q: &Query) -> Vec<(usize, String)> {
    let mut sites = LockSites { seen: 0, out: Vec::new() };
    let _ = q.visit(&mut sites);
    sites.out
}

struct LockSites {
    seen: usize,
    out: Vec<(usize, String)>,
}

impl Visitor for LockSites {
    type Break = ();

    fn pre_visit_query(&mut self, q: &Query) -> ControlFlow<Self::Break> {
        self.out.extend(q.locks.iter().map(|l| (self.seen, l.to_string())));
        self.seen += 1;
        ControlFlow::Continue(())
    }
}

struct ClearLocks;

impl VisitorMut for ClearLocks {
    type Break = ();

    fn pre_visit_query(&mut self, q: &mut Query) -> ControlFlow<Self::Break> {
        q.locks.clear();
        ControlFlow::Continue(())
    }
}

/// Replace every `WITH` binding with a derived table at each of its uses, innermost first.
///
/// ```text
/// WITH c AS (SELECT a FROM t) SELECT * FROM c
///   becomes   SELECT * FROM (SELECT a FROM t) AS c
/// ```
///
/// A `WITH` binding *is* a derived table that has been given a name and hoisted, so substituting the
/// definition back into each use returns it to the form it is sugar for. The prover's IR has no
/// binding construct at all — [`infer`][crate::infer] refuses a query that still carries a `WITH` —
/// so without this every CTE-bearing pair is unlowerable — which on real rewrite pairs is the largest
/// single refusal bucket there is.
///
/// # Preconditions
///
/// **`RECURSIVE` is refused.** A recursive CTE denotes a fixpoint, and no finite substitution of its
/// body for its name computes one. The flag is on the `WITH` rather than the individual binding, so
/// `WITH RECURSIVE` blocks the whole clause even when nothing in it actually self-references — the
/// conservative direction, and the flag is what a reader of the query has to go on too.
///
/// **A data-modifying binding is refused.** `WITH x AS (INSERT … RETURNING …)` has an effect as well
/// as a value; duplicating it at N uses would duplicate the effect. sqlparser parses these as
/// [`SetExpr::Insert`]/[`Update`][SetExpr::Update]/[`Delete`][SetExpr::Delete]/[`Merge`][SetExpr::Merge],
/// so the check is on the body's shape and not on a keyword.
///
/// **Only a bare, unqualified, single-part name is a use.** `s.c` is a table in schema `s`, never a
/// reference to CTE `c`, and `c(1, 2)` is a table-function call that happens to share a name. This is
/// also why this rewrite has to run *before* [`strip_schema`]: strip the qualifier off `part_16.c`
/// first and it becomes indistinguishable from a use of a CTE named `c`, which would substitute a
/// definition for a reference to a real table.
///
/// # What is not a precondition, and why
///
/// **Multiple uses.** Inlining a binding used N times evaluates its body N times, and that is the
/// same value each time: [`lower`][crate::lower] refuses every function whose result can differ
/// between two calls with equal arguments (`random`, `nextval`, `clock_timestamp` — see its
/// `NONDETERMINISTIC`). With those gone, "how many times" is not an observable.
///
/// **`MATERIALIZED`.** Same argument. The hint controls whether the planner evaluates the body once
/// into a temporary or folds it into each use; for a body whose value is a function of the input
/// tables, both compute the same relation. It is a performance annotation, and this frontend has no
/// performance to annotate.
///
/// **A column-alias list.** `WITH c(x, y) AS (SELECT a, b FROM t)` renames the output, and the rename
/// is carried onto the derived table's alias, where [`lower`][crate::lower] applies it. Dropping it
/// instead would be a silent wrong answer rather than a refusal, because the underlying names are
/// still there to resolve against: over `WITH c(b, a) AS (SELECT a, b FROM t) SELECT a FROM c`, the
/// query returns `t.b` and the naively-inlined form returns `t.a`.
///
/// # A note on what this does to a rewrite corpus
///
/// Some real pairs *are* a CTE against its inlined form — that is a real optimization and
/// one of the things worth proving. Inlining both sides makes such a pair reflexive, and the proof
/// that follows is a proof about a query against itself. It is still a correct verdict for the pair;
/// it is just not evidence that the prover can reason about `WITH`. So a run reporting how much it
/// proved should split that by whether the two sides survived normalization distinct, or the figure
/// flatters the prover.
pub fn inline_ctes(queries: &mut [Query]) {
    for q in queries {
        let _ = q.visit(&mut InlineCtes);
    }
}

struct InlineCtes;

impl VisitorMut for InlineCtes {
    type Break = ();

    /// Post-order, so a nested `WITH` is already gone by the time its enclosing one is inlined. That
    /// is what keeps shadowing right — an inner binding of the same name has consumed its own uses
    /// before the outer definition goes looking for them — and it is also why no fixpoint is needed:
    /// a definition is only ever copied after its own bindings have been substituted away.
    fn post_visit_query(&mut self, q: &mut Query) -> ControlFlow<Self::Break> {
        let Some(with) = q.with.take() else {
            return ControlFlow::Continue(());
        };
        if with.recursive || !with.cte_tables.iter().all(inlinable) {
            // Put it back and stop: a surviving `WITH` is refused downstream, which is the outcome
            // wanted here. Anything already inlined deeper in the tree stays inlined and is harmless,
            // since this one binding is enough to refuse the query.
            q.with = Some(with);
            return ControlFlow::Break(());
        }
        let mut defs: HashMap<String, Cte> = HashMap::new();
        for mut cte in with.cte_tables {
            // A binding may use the ones before it in the same `WITH`, so each definition is closed
            // over its predecessors before being recorded as one itself.
            let _ = VisitMut::visit(&mut *cte.query, &mut ReplaceCteRefs(&defs));
            defs.insert(cte.alias.name.value.to_lowercase(), cte);
        }
        let _ = q.visit(&mut ReplaceCteRefs(&defs));
        ControlFlow::Continue(())
    }
}

/// Can this binding be replaced by its definition at all?
fn inlinable(cte: &Cte) -> bool {
    // `WITH x AS (...) FROM y` — a ClickHouse form whose scoping is not the one argued above.
    if cte.from.is_some() {
        return false;
    }
    // A binding that writes. Duplicating a value is free; duplicating an effect is not.
    !matches!(
        &*cte.query.body,
        SetExpr::Insert(_) | SetExpr::Update(_) | SetExpr::Delete(_) | SetExpr::Merge(_)
    )
}

struct ReplaceCteRefs<'a>(&'a HashMap<String, Cte>);

impl VisitorMut for ReplaceCteRefs<'_> {
    type Break = ();

    /// Post-order again, and here it is load-bearing rather than tidy: the walker descends into a
    /// factor *before* this fires, so it never re-enters the definition just substituted in. A use
    /// inside a definition of the same name refers to something else and has to survive —
    /// `WITH a AS (SELECT * FROM a)` reads the base table `a`, since a non-recursive binding is not in
    /// scope within itself. Re-descending would substitute that away, and would not terminate.
    fn post_visit_table_factor(&mut self, tf: &mut TableFactor) -> ControlFlow<Self::Break> {
        let TableFactor::Table { name, alias, args, sample, .. } = tf else {
            return ControlFlow::Continue(());
        };
        // `c(...)` is a table-function call, and `c TABLESAMPLE ...` samples a real table. Neither is
        // a plain reference, whatever the name says.
        if args.is_some() || sample.is_some() {
            return ControlFlow::Continue(());
        }
        let [part] = &name.0[..] else {
            return ControlFlow::Continue(());
        };
        let Some(ident) = part.as_ident() else {
            return ControlFlow::Continue(());
        };
        let Some(cte) = self.0.get(&ident.value.to_lowercase()) else {
            return ControlFlow::Continue(());
        };
        // `AS b AT i` is a PartiQL index alias over a nested array, which is not what a `WITH` binding
        // is being used as. Leave it and let the unknown table be refused.
        if alias.as_ref().is_some_and(|a| a.at.is_some()) {
            return ControlFlow::Continue(());
        }
        // The use's own alias wins over the binding's name, and its own column list -- if it has one
        // -- over the binding's, exactly as a second `AS c(x, y)` on a derived table would.
        let alias = match alias.take() {
            Some(mut a) => {
                if a.columns.is_empty() {
                    a.columns = cte.alias.columns.clone();
                }
                a
            }
            None => TableAlias {
                explicit: true,
                name: ident.clone(),
                columns: cte.alias.columns.clone(),
                at: None,
            },
        };
        *tf = TableFactor::Derived {
            lateral: false,
            subquery: cte.query.clone(),
            alias: Some(alias),
            sample: None,
        };
        ControlFlow::Continue(())
    }
}

/// Schemas whose contents are the server's, not the user's. A table under one of these is not the
/// table the DDL declares even if the bare names collide.
const SYS_SCHEMAS: [&str; 4] = ["pg_catalog", "information_schema", "pg_temp", "sys"];

/// Drop the schema/database qualifier from table references: `part_16.orders` -> `orders`.
///
/// The corpus's queries name their tables through a shard or tenant schema while the DDL collected
/// alongside them declares the bare name. Nothing then resolves, and the pair is refused for having
/// no base tables at all — so this is a *naming* fix, not a semantic rewrite: it makes two spellings
/// of the same table agree.
///
/// It is still a rewrite that can be wrong, in one specific way, so it is all-or-nothing per query:
///
/// * **A system schema anywhere stops it.** `pg_catalog.x` is not the user's `x`, and folding one
///   into the other would silently answer a question about the wrong table. An unresolved reference
///   downstream is a refusal; a silent rename is not.
/// * **A bare name reached through two different qualifiers stops it.** `a.orders` and `b.orders`
///   are two tables, and stripping would merge them into one — turning a join between two relations
///   into a self-join, which changes the answer rather than the spelling.
///
/// Column qualifiers are left as they are. A three-part `part_16.orders.id` resolves on its last
/// two parts, so once the table is bare the column already matches it.
pub fn strip_schema(queries: &mut [Query]) {
    for q in queries {
        let mut names = CollectTableNames(Vec::new());
        // `&*q` so this picks the read-only `Visit`, not `VisitMut`: the guards have to see every
        // table reference before the first one is rewritten.
        let _ = Visit::visit(&*q, &mut names);
        if !safe_to_strip(&names.0) {
            continue;
        }
        let _ = q.visit(&mut StripQualifier);
    }
}

struct CollectTableNames(Vec<ObjectName>);

impl Visitor for CollectTableNames {
    type Break = ();

    fn pre_visit_relation(&mut self, name: &ObjectName) -> ControlFlow<Self::Break> {
        self.0.push(name.clone());
        ControlFlow::Continue(())
    }
}

struct StripQualifier;

impl VisitorMut for StripQualifier {
    type Break = ();

    fn pre_visit_relation(&mut self, name: &mut ObjectName) -> ControlFlow<Self::Break> {
        if name.0.len() > 1 {
            name.0.drain(..name.0.len() - 1);
        }
        ControlFlow::Continue(())
    }
}

/// The two guards, checked over every table reference in one query before any of them is touched.
fn safe_to_strip(names: &[ObjectName]) -> bool {
    let part = |n: &ObjectName, i: usize| {
        n.0.get(i).and_then(|p| p.as_ident()).map(|id| id.value.to_lowercase())
    };
    let mut seen: HashMap<String, String> = HashMap::new();
    for n in names {
        let Some(bare) = part(n, n.0.len() - 1) else { return false };
        if bare.starts_with("pg_") {
            return false;
        }
        let qualifier = n.0[..n.0.len() - 1]
            .iter()
            .map(|p| p.as_ident().map(|id| id.value.to_lowercase()).unwrap_or_default())
            .collect::<Vec<_>>();
        if qualifier.iter().any(|q| SYS_SCHEMAS.contains(&q.as_str())) {
            return false;
        }
        // Two spellings of one bare name are two tables until proven otherwise.
        match seen.entry(bare) {
            Entry::Occupied(e) if *e.get() != qualifier.join(".") => return false,
            Entry::Occupied(_) => {}
            Entry::Vacant(e) => {
                e.insert(qualifier.join("."));
            }
        }
    }
    true
}

/// Is every `ORDER BY` key of this query recoverable from the rows it returns?
///
/// Conservative by construction: it answers yes only for keys it can *match* to the projection, so
/// an ordering it cannot analyse blocks the strip rather than being assumed harmless.
fn order_determined_by_projection(q: &Query) -> bool {
    let Some(order_by) = &q.order_by else { return true };
    let OrderByKind::Expressions(keys) = &order_by.kind else { return false };
    let SetExpr::Select(select) = &*q.body else { return false };

    let mut projected: Vec<String> = Vec::new();
    for item in &select.projection {
        match item {
            // A star projects everything the sources have, so any key over those sources is
            // recoverable — but working out *which* columns those are is name resolution, which has
            // not run yet. Refuse rather than guess.
            SelectItem::Wildcard(_) | SelectItem::QualifiedWildcard(..) => return false,
            SelectItem::UnnamedExpr(e) => projected.push(e.to_string()),
            SelectItem::ExprWithAlias { expr, alias } => {
                projected.push(expr.to_string());
                projected.push(alias.value.clone());
            }
            // A Spark multi-alias projection expands one expression into several output columns;
            // which key maps to which is not something this needs to work out to refuse.
            SelectItem::ExprWithAliases { .. } => return false,
        }
    }

    keys.iter().all(|key| {
        // `ORDER BY 2` is the second output column by position — determined whenever it is in range.
        if let Expr::Value(v) = &key.expr {
            if let sqlparser::ast::Value::Number(n, _) = &v.value {
                return n.parse::<usize>().is_ok_and(|i| i >= 1 && i <= select.projection.len());
            }
        }
        let text = key.expr.to_string();
        projected.contains(&text)
    })
}

#[cfg(test)]
mod tests {
    use sqlparser::parser::Parser;

    use super::*;

    /// Parse one expression, fix it, and render it back with explicit parentheses implied by shape.
    fn fixed(sql: &str) -> String {
        let mut st = Parser::parse_sql(&crate::DIALECT, &format!("SELECT 1 WHERE {sql}"))
            .expect("parses");
        fix_precedence(&mut st).expect("no refusal");
        shape(selection(&st))
    }

    fn err(sql: &str) -> String {
        let mut st = Parser::parse_sql(&crate::DIALECT, &format!("SELECT 1 WHERE {sql}"))
            .expect("parses");
        match fix_precedence(&mut st) {
            Err(e) => e.to_string(),
            Ok(()) => panic!("should refuse: {sql}"),
        }
    }

    fn selection(st: &[Statement]) -> &Expr {
        let Statement::Query(q) = &st[0] else { panic!("query") };
        let sqlparser::ast::SetExpr::Select(s) = &*q.body else { panic!("select") };
        s.selection.as_ref().expect("where")
    }

    /// Fully-parenthesised rendering, so the assertions are about tree shape and not about how
    /// `Display` chooses to print it.
    fn shape(e: &Expr) -> String {
        match e {
            Expr::BinaryOp { left, op, right } => {
                format!("({} {op} {})", shape(left), shape(right))
            }
            Expr::IsDistinctFrom(a, b) => format!("({} IDF {})", shape(a), shape(b)),
            Expr::IsNotDistinctFrom(a, b) => format!("({} INDF {})", shape(a), shape(b)),
            Expr::Nested(x) => format!("[{}]", shape(x)),
            other => other.to_string(),
        }
    }

    // The next three pin the parser: these are the inputs sqlparser 0.62 mis-parsed. Should a
    // release bring the bug back, the guard refuses them and `fixed` panics here.

    #[test]
    fn conjunction_stays_outside_the_comparison() {
        assert_eq!(fixed("a IS DISTINCT FROM 1 AND b = 2"), "((a IDF 1) AND (b = 2))");
        assert_eq!(fixed("a IS NOT DISTINCT FROM 1 OR b = 2"), "((a INDF 1) OR (b = 2))");
    }

    #[test]
    fn and_or_precedence_is_kept_around_the_comparison() {
        assert_eq!(fixed("a IS DISTINCT FROM 1 OR b AND c"), "((a IDF 1) OR (b AND c))");
        assert_eq!(fixed("a IS DISTINCT FROM 1 AND b OR c"), "(((a IDF 1) AND b) OR c)");
    }

    #[test]
    fn nested_comparisons_parse_as_a_left_associative_conjunction() {
        assert_eq!(
            fixed("a IS DISTINCT FROM 1 AND b IS DISTINCT FROM 2 AND c"),
            "(((a IDF 1) AND (b IDF 2)) AND c)"
        );
    }

    #[test]
    fn parentheses_are_left_alone() {
        assert_eq!(fixed("a IS DISTINCT FROM (1 AND b)"), "(a IDF [(1 AND b)])");
    }

    #[test]
    fn already_correct_trees_are_untouched() {
        assert_eq!(fixed("a IS DISTINCT FROM 1"), "(a IDF 1)");
        assert_eq!(fixed("a = 1 AND b IS DISTINCT FROM 2"), "((a = 1) AND (b IDF 2))");
        assert_eq!(fixed("a IS DISTINCT FROM b + 1"), "(a IDF (b + 1))");
    }

    #[test]
    fn chained_is_operators_are_refused_in_either_nesting() {
        assert!(err("a IS DISTINCT FROM b IS NULL").contains("non-associative"));
        assert!(err("a IS NULL IS DISTINCT FROM b").contains("non-associative"));
        assert!(err("a IS DISTINCT FROM b IS NOT DISTINCT FROM c").contains("non-associative"));
        assert_eq!(fixed("(a IS DISTINCT FROM b) IS NULL"), "(a IS DISTINCT FROM b) IS NULL");
    }

    #[test]
    fn a_not_operand_is_refused() {
        assert!(err("a IS DISTINCT FROM NOT b").contains("NOT"));
    }

    /// The 0.62 tree, built by hand because no parser in use produces it any more: without this the
    /// guard's first branch would have no test that can fail.
    #[test]
    fn the_old_mis_parse_tree_is_refused() {
        let mut st =
            Parser::parse_sql(&crate::DIALECT, "SELECT 1 WHERE a IS DISTINCT FROM (1 AND b)")
                .expect("parses");
        let Statement::Query(q) = &mut st[0] else { panic!("query") };
        let sqlparser::ast::SetExpr::Select(s) = &mut *q.body else { panic!("select") };
        let Some(Expr::IsDistinctFrom(_, r)) = s.selection.as_mut() else { panic!("idf") };
        let Expr::Nested(inner) = std::mem::replace(&mut **r, placeholder()) else {
            panic!("nested")
        };
        **r = *inner;
        let e = fix_precedence(&mut st).expect_err("the bare AND/OR shape must be refused");
        assert!(e.to_string().contains("0.62 mis-parse"), "{e}");
    }

    /// Round-trip one statement through the strip. The cases below are this rewrite's
    /// specification.
    fn stripped(sql: &str) -> String {
        let mut st = Parser::parse_sql(&crate::DIALECT, sql).expect("parses");
        strip_in_exists_distinct(&mut st);
        st[0].to_string()
    }

    /// Unchanged means the guard held. Spelled as its own helper so the negative cases read as
    /// "this is not rewritten" rather than as a string comparison against themselves.
    fn unchanged(sql: &str) {
        assert_eq!(stripped(sql), sql, "should not be rewritten");
    }

    #[test]
    fn strips_distinct_in_the_two_subquery_positions() {
        assert_eq!(
            stripped("SELECT x FROM a WHERE x IN (SELECT DISTINCT e FROM t)"),
            "SELECT x FROM a WHERE x IN (SELECT e FROM t)"
        );
        assert_eq!(
            stripped("SELECT x FROM a WHERE EXISTS (SELECT DISTINCT e FROM t)"),
            "SELECT x FROM a WHERE EXISTS (SELECT e FROM t)"
        );
        assert_eq!(
            stripped("SELECT x FROM a WHERE x NOT IN (SELECT DISTINCT e FROM t)"),
            "SELECT x FROM a WHERE x NOT IN (SELECT e FROM t)"
        );
    }

    /// A `DISTINCT` anywhere else is observable and stays: only the `IN`/`EXISTS` position is one
    /// where multiplicity cannot be seen.
    #[test]
    fn leaves_distinct_alone_outside_those_positions() {
        unchanged("SELECT DISTINCT x FROM a WHERE x IN (SELECT e FROM t)");
        unchanged("SELECT x FROM (SELECT DISTINCT e AS x FROM t) s");
    }

    /// t = {1, 1, 2}: `DISTINCT … LIMIT 2` is {1, 2}, `LIMIT 2` alone is {1}. `FETCH FIRST` is a
    /// separate field on the query and has to be checked separately from `LIMIT`.
    #[test]
    fn keeps_distinct_under_a_row_slice() {
        unchanged("SELECT x FROM a WHERE x IN (SELECT DISTINCT e FROM t ORDER BY e LIMIT 2)");
        unchanged("SELECT x FROM a WHERE x IN (SELECT DISTINCT e FROM t ORDER BY e OFFSET 1)");
        unchanged("SELECT x FROM a WHERE x IN (SELECT DISTINCT e FROM t LIMIT 2 OFFSET 1)");
        unchanged(
            "SELECT x FROM a WHERE x IN (SELECT DISTINCT e FROM t FETCH FIRST 2 ROWS ONLY)",
        );
        // Uniform over EXISTS, where a bare LIMIT would in fact be harmless.
        unchanged("SELECT x FROM a WHERE EXISTS (SELECT DISTINCT e FROM t OFFSET 2)");
        unchanged("SELECT x FROM a WHERE EXISTS (SELECT DISTINCT e FROM t LIMIT 1)");
    }

    /// t = {(1,10), (1,20)}: `DISTINCT ON (k)` yields {10}, no modifier yields {10, 20}. It is a
    /// row-dropping operator wearing the same keyword.
    #[test]
    fn keeps_distinct_on() {
        unchanged("SELECT x FROM a WHERE x IN (SELECT DISTINCT ON (t.k) t.e FROM t ORDER BY t.k)");
        unchanged("SELECT x FROM a WHERE EXISTS (SELECT DISTINCT ON (t.k) t.e FROM t)");
    }

    /// Both guards are per-subquery. A blocked one does not freeze its siblings — a slice nested
    /// deeper runs before anything at this level and cannot see what is removed here.
    #[test]
    fn guards_are_per_subquery() {
        assert_eq!(
            stripped(
                "SELECT x FROM a WHERE x IN (SELECT DISTINCT e FROM t LIMIT 2) \
                 AND x IN (SELECT DISTINCT e FROM u)"
            ),
            "SELECT x FROM a WHERE x IN (SELECT DISTINCT e FROM t LIMIT 2) \
             AND x IN (SELECT e FROM u)"
        );
        assert_eq!(
            stripped(
                "SELECT x FROM a WHERE x IN \
                 (SELECT DISTINCT e FROM (SELECT v AS e FROM t LIMIT 3) s)"
            ),
            "SELECT x FROM a WHERE x IN (SELECT e FROM (SELECT v AS e FROM t LIMIT 3) s)"
        );
    }

    /// An explicit `ALL` is not a `DISTINCT` and there is nothing to remove; touching it would be a
    /// rendering change with no equivalence argument behind it.
    #[test]
    fn explicit_all_is_left_alone() {
        unchanged("SELECT x FROM a WHERE x IN (SELECT ALL e FROM t)");
    }

    // -----------------------------------------------------------------------
    // unnest_in_to_any
    // -----------------------------------------------------------------------

    /// Round-trip one statement through the `IN (SELECT unnest(A))` -> `= ANY(A)` rewrite.
    fn anyed(sql: &str) -> String {
        let mut st = Parser::parse_sql(&crate::DIALECT, sql).expect("parses");
        unnest_in_to_any(&mut st);
        st[0].to_string()
    }

    /// The negative form: the guard held and the statement is untouched.
    fn not_anyed(sql: &str) {
        assert_eq!(anyed(sql), sql, "should not be rewritten");
    }

    #[test]
    fn rewrites_in_the_three_filter_positions() {
        assert_eq!(
            anyed("SELECT x FROM a WHERE x IN (SELECT unnest($1))"),
            "SELECT x FROM a WHERE x = ANY($1)"
        );
        assert_eq!(
            anyed("SELECT k FROM a GROUP BY k HAVING max(x) IN (SELECT unnest($1))"),
            "SELECT k FROM a GROUP BY k HAVING max(x) = ANY($1)"
        );
        assert_eq!(
            anyed("SELECT x FROM a JOIN b ON b.id IN (SELECT unnest($1))"),
            "SELECT x FROM a JOIN b ON b.id = ANY($1)"
        );
        // An outer join's ON is still a filter for this purpose: a row whose ON is not TRUE is
        // null-extended, and NULL and FALSE are both not TRUE.
        assert_eq!(
            anyed("SELECT x FROM a LEFT JOIN b ON b.id IN (SELECT unnest($1))"),
            "SELECT x FROM a LEFT JOIN b ON b.id = ANY($1)"
        );
    }

    /// The connectives the induction covers, and only those.
    #[test]
    fn descends_through_and_or_and_parens() {
        assert_eq!(
            anyed("SELECT x FROM a WHERE y = 1 AND (x IN (SELECT unnest($1)) OR z = 2)"),
            "SELECT x FROM a WHERE y = 1 AND (x = ANY($1) OR z = 2)"
        );
    }

    /// Every context where NULL and FALSE are told apart. Each of these would be a false proof
    /// against the `= ANY` spelling if the rewrite fired: `x = ANY(NULL::int[])` is NULL while
    /// `x IN (SELECT unnest(NULL::int[]))` is FALSE.
    #[test]
    fn refuses_outside_a_positive_filter() {
        not_anyed("SELECT x FROM a WHERE NOT (x IN (SELECT unnest($1)))");
        not_anyed("SELECT x FROM a WHERE (x IN (SELECT unnest($1))) IS NULL");
        not_anyed("SELECT x FROM a WHERE (x IN (SELECT unnest($1))) IS NOT NULL");
        not_anyed("SELECT x IN (SELECT unnest($1)) FROM a");
        not_anyed("SELECT x FROM a WHERE CASE WHEN x IN (SELECT unnest($1)) THEN 1 ELSE 2 END = 1");
        not_anyed("SELECT x FROM a WHERE coalesce(x IN (SELECT unnest($1)), false)");
        not_anyed("SELECT bool_and(x IN (SELECT unnest($1))) FROM a");
    }

    /// `NOT IN` is the negation and inverts which of NULL/FALSE passes the filter. No corpus row
    /// spells it, so the guard is free.
    #[test]
    fn refuses_not_in() {
        not_anyed("SELECT x FROM a WHERE x NOT IN (SELECT unnest($1))");
    }

    /// The subquery has to be *exactly* `SELECT [DISTINCT] unnest(X)`. The rewrite discards the
    /// query and keeps the one argument, so any surviving clause would be a clause dropped.
    #[test]
    fn refuses_anything_but_a_bare_unnest_projection() {
        not_anyed("SELECT x FROM a WHERE x IN (SELECT unnest(t.c) FROM t)");
        not_anyed("SELECT x FROM a WHERE x IN (SELECT unnest($1) WHERE $2)");
        not_anyed("SELECT x FROM a WHERE x IN (SELECT unnest($1) LIMIT 1)");
        not_anyed("SELECT x FROM a WHERE x IN (SELECT unnest($1) OFFSET 1)");
        not_anyed("SELECT x FROM a WHERE x IN (SELECT unnest($1) FETCH FIRST 1 ROWS ONLY)");
        not_anyed("SELECT x FROM a WHERE x IN (SELECT unnest($1) ORDER BY 1)");
        not_anyed("SELECT x FROM a WHERE x IN (SELECT unnest($1), 1)");
        not_anyed("SELECT x FROM a WHERE x IN (WITH c AS (SELECT 1) SELECT unnest($1))");
        not_anyed("SELECT x FROM a WHERE x IN (SELECT unnest($1) UNION SELECT unnest($2))");
        not_anyed("SELECT x FROM a WHERE x IN (SELECT DISTINCT ON (1) unnest($1))");
        // The multi-array form pairs elements by position and has no `= ANY` equivalent.
        not_anyed("SELECT x FROM a WHERE x IN (SELECT unnest($1, $2))");
        // Another set-returning function is not unnest, and its refusal in `lower` stands.
        not_anyed("SELECT x FROM a WHERE x IN (SELECT generate_series(1, $1))");
    }

    /// `strip_in_exists_distinct` normally removes this first; accepting it makes the two passes
    /// order-independent. De-duplicating a bag before `IN` is not observable.
    #[test]
    fn accepts_a_bare_distinct_for_order_independence() {
        assert_eq!(
            anyed("SELECT x FROM a WHERE x IN (SELECT DISTINCT unnest($1))"),
            "SELECT x FROM a WHERE x = ANY($1)"
        );
    }

    /// An alias is not observable through `IN`, and a qualified name is still unnest — matched on
    /// the bare name, as `lower`'s own `SET_RETURNING` guard is.
    #[test]
    fn alias_and_qualification_do_not_block_it() {
        assert_eq!(
            anyed("SELECT x FROM a WHERE x IN (SELECT unnest($1) AS e)"),
            "SELECT x FROM a WHERE x = ANY($1)"
        );
        assert_eq!(
            anyed("SELECT x FROM a WHERE x IN (SELECT pg_catalog.unnest($1))"),
            "SELECT x FROM a WHERE x = ANY($1)"
        );
    }

    /// Reached wherever a query is, not just at the top: inside a CTE, a derived table, a set
    /// operation and a correlated subquery's own filter.
    #[test]
    fn reaches_nested_queries() {
        assert_eq!(
            anyed("WITH c AS (SELECT x FROM a WHERE x IN (SELECT unnest($1))) SELECT * FROM c"),
            "WITH c AS (SELECT x FROM a WHERE x = ANY($1)) SELECT * FROM c"
        );
        assert_eq!(
            anyed("SELECT * FROM (SELECT x FROM a WHERE x IN (SELECT unnest($1))) s"),
            "SELECT * FROM (SELECT x FROM a WHERE x = ANY($1)) s"
        );
        assert_eq!(
            anyed(
                "SELECT x FROM a WHERE x IN (SELECT unnest($1)) \
                 UNION SELECT y FROM b WHERE y IN (SELECT unnest($2))"
            ),
            "SELECT x FROM a WHERE x = ANY($1) UNION SELECT y FROM b WHERE y = ANY($2)"
        );
        assert_eq!(
            anyed(
                "SELECT x FROM a WHERE EXISTS \
                 (SELECT 1 FROM b WHERE b.id IN (SELECT unnest($1)))"
            ),
            "SELECT x FROM a WHERE EXISTS (SELECT 1 FROM b WHERE b.id = ANY($1))"
        );
    }

    /// The cast and array-literal spellings the corpus actually contains, carried across verbatim.
    #[test]
    fn carries_the_argument_across_unchanged() {
        assert_eq!(
            anyed("SELECT x FROM a WHERE x IN (SELECT unnest($1::uuid[]))"),
            "SELECT x FROM a WHERE x = ANY($1::UUID[])"
        );
        assert_eq!(
            anyed("SELECT x FROM a WHERE x IN (SELECT unnest(ARRAY[$1]::bigint[]))"),
            "SELECT x FROM a WHERE x = ANY(ARRAY[$1]::BIGINT[])"
        );
        assert_eq!(
            anyed("SELECT x FROM a WHERE x IN (SELECT unnest(t.ids))"),
            "SELECT x FROM a WHERE x = ANY(t.ids)"
        );
    }

    fn parse_queries(a: &str, b: &str) -> Vec<Query> {
        [a, b]
            .iter()
            .map(|sql| {
                let st = Parser::parse_sql(&crate::DIALECT, sql).expect("parses");
                match st.into_iter().next().expect("one statement") {
                    Statement::Query(q) => *q,
                    other => panic!("not a query: {other}"),
                }
            })
            .collect()
    }

    /// Run the pair-level strip and render both sides back.
    fn paginated(a: &str, b: &str) -> (String, String) {
        let mut qs = parse_queries(a, b);
        strip_identical_pagination(&mut qs);
        (qs[0].to_string(), qs[1].to_string())
    }

    /// The strip did not fire: both sides came back as they went in.
    fn pagination_kept(a: &str, b: &str) {
        assert_eq!(paginated(a, b), (a.to_string(), b.to_string()), "should not be rewritten");
    }

    #[test]
    fn strips_pagination_identical_on_both_sides() {
        assert_eq!(
            paginated(
                "SELECT a FROM t WHERE x = 1 ORDER BY a LIMIT 10 OFFSET 5",
                "SELECT a FROM t WHERE 1 = x ORDER BY a LIMIT 10 OFFSET 5",
            ),
            ("SELECT a FROM t WHERE x = 1".into(), "SELECT a FROM t WHERE 1 = x".into())
        );
    }

    /// Differing pagination is a real difference between the two queries, not noise to normalize
    /// away — it stays, and the pair is refused downstream for carrying a `LIMIT`.
    #[test]
    fn keeps_pagination_that_differs() {
        pagination_kept("SELECT a FROM t ORDER BY a LIMIT 10", "SELECT a FROM t ORDER BY a LIMIT 20");
        pagination_kept("SELECT a FROM t ORDER BY a LIMIT 10", "SELECT a FROM t ORDER BY b LIMIT 10");
        pagination_kept(
            "SELECT a FROM t ORDER BY a LIMIT 10 OFFSET 1",
            "SELECT a FROM t ORDER BY a LIMIT 10",
        );
    }

    /// No slice means nothing is paginating: the `ORDER BY` is dead and belongs to the other strip,
    /// which is tree-wide rather than pair-level.
    #[test]
    fn pagination_strip_needs_a_slice() {
        pagination_kept("SELECT a FROM t ORDER BY a", "SELECT a FROM t ORDER BY a");
    }

    /// The counterexample from the doc comment. `SELECT a FROM t` and `SELECT a FROM u` can be the
    /// same bag while the two pages differ, because `b` is not in the output.
    #[test]
    fn keeps_pagination_ordered_by_a_column_the_projection_drops() {
        pagination_kept("SELECT a FROM t ORDER BY b LIMIT 1", "SELECT a FROM t ORDER BY b LIMIT 1");
        pagination_kept(
            "SELECT a FROM t ORDER BY a, b LIMIT 1",
            "SELECT a FROM t ORDER BY a, b LIMIT 1",
        );
    }

    /// Ordinals, aliases and repeated expressions all name something the output carries, so the bag
    /// determines the page.
    #[test]
    fn accepts_order_keys_the_projection_determines() {
        let (a, _) = paginated(
            "SELECT a, b FROM t ORDER BY 2, a LIMIT 3",
            "SELECT a, b FROM t ORDER BY 2, a LIMIT 3",
        );
        assert_eq!(a, "SELECT a, b FROM t");
        let (a, _) = paginated(
            "SELECT lower(n) AS k FROM t ORDER BY k LIMIT 3",
            "SELECT lower(n) AS k FROM t ORDER BY k LIMIT 3",
        );
        assert_eq!(a, "SELECT lower(n) AS k FROM t");
        let (a, _) = paginated(
            "SELECT lower(n) AS k FROM t ORDER BY lower(n) LIMIT 3",
            "SELECT lower(n) AS k FROM t ORDER BY lower(n) LIMIT 3",
        );
        assert_eq!(a, "SELECT lower(n) AS k FROM t");
    }

    /// The condition is a question about one query's own projection, so it gets asked of both sides.
    /// Identical clause text fixes the key's *spelling* and not its referent: here `ORDER BY y` is
    /// the output of A and the unprojected `t.b` of B, the stripped pair is provable because both
    /// return the bag of `t.a`, and over `t = {(a=1,b=2), (a=2,b=1)}` A yields 1 while B yields 2.
    /// Checking only side A stripped this pair.
    #[test]
    fn keeps_pagination_when_only_one_sides_order_is_determined() {
        let a = "SELECT y FROM (SELECT a AS y FROM t) AS v ORDER BY y LIMIT 1";
        let b = "SELECT x FROM (SELECT a AS x, b AS y FROM t) AS v ORDER BY y LIMIT 1";
        pagination_kept(a, b);
        // Neither position is privileged: the same pair with the sides swapped is also kept.
        pagination_kept(b, a);
    }

    /// An out-of-range ordinal is not a column of the output; and `SELECT *` needs name resolution
    /// to know what the output even is, which has not run yet.
    #[test]
    fn refuses_order_keys_it_cannot_match() {
        pagination_kept("SELECT a FROM t ORDER BY 2 LIMIT 1", "SELECT a FROM t ORDER BY 2 LIMIT 1");
        pagination_kept("SELECT * FROM t ORDER BY b LIMIT 1", "SELECT * FROM t ORDER BY b LIMIT 1");
    }

    fn ordered(sql: &str) -> String {
        let mut qs = parse_queries(sql, sql);
        strip_dead_order_by(&mut qs);
        qs[0].to_string()
    }

    #[test]
    fn drops_order_by_with_no_slice_downstream() {
        assert_eq!(ordered("SELECT a FROM t ORDER BY a"), "SELECT a FROM t");
        assert_eq!(
            ordered("SELECT a FROM (SELECT a FROM t ORDER BY a) s ORDER BY a"),
            "SELECT a FROM (SELECT a FROM t) s"
        );
    }

    /// Tree-wide: one slice anywhere is enough, because the strip would otherwise clear the very
    /// ordering that chooses the rows that slice keeps.
    #[test]
    fn keeps_every_order_by_when_a_slice_survives_anywhere() {
        let sql = "SELECT a FROM (SELECT a FROM t ORDER BY a LIMIT 5) s ORDER BY a";
        assert_eq!(ordered(sql), sql);
    }

    /// `DISTINCT ON` consumes an ordering without leaving a `Sort` behind: the clause is encoded
    /// into the name of an opaque operator instead (`lower::distinct_on`). Stripping it here would
    /// make "largest `t` per key" and "smallest `t` per key" the same operator.
    #[test]
    fn keeps_every_order_by_when_a_distinct_on_survives_anywhere() {
        for sql in [
            "SELECT DISTINCT ON (k) k, v FROM ev ORDER BY k, t DESC",
            "SELECT a FROM (SELECT DISTINCT ON (k) k FROM ev ORDER BY k, t) s ORDER BY a",
            "SELECT DISTINCT ON (k) k FROM ev UNION SELECT k FROM ev ORDER BY k",
        ] {
            assert_eq!(ordered(sql), sql, "the ordering a DISTINCT ON reads must survive");
        }
    }

    /// The same clause is also not the page's ordering alone, so the identical-pagination strip has
    /// to leave it: a `LIMIT` on top does not make the row *choice* immaterial.
    #[test]
    fn keeps_identical_pagination_over_a_distinct_on() {
        let q = "SELECT DISTINCT ON (k) k, v FROM ev ORDER BY k, t DESC LIMIT 5";
        pagination_kept(q, q);
    }

    /// A window's `ORDER BY` is part of the window function's value, not a query-level ordering.
    #[test]
    fn leaves_window_ordering_alone() {
        assert_eq!(
            ordered("SELECT row_number() OVER (ORDER BY a) FROM t ORDER BY a"),
            "SELECT row_number() OVER (ORDER BY a) FROM t"
        );
    }

    fn unschemad(sql: &str) -> String {
        let mut qs = parse_queries(sql, sql);
        strip_schema(&mut qs);
        qs[0].to_string()
    }

    #[test]
    fn strips_the_schema_qualifier_everywhere_in_a_query() {
        assert_eq!(
            unschemad(
                "SELECT part_16.orders.id FROM part_16.orders \
                 JOIN part_16.lines ON part_16.lines.oid = part_16.orders.id"
            ),
            "SELECT part_16.orders.id FROM orders JOIN lines ON part_16.lines.oid = part_16.orders.id"
        );
        // Three-part table names lose both leading parts.
        assert_eq!(unschemad("SELECT a FROM db.public.t"), "SELECT a FROM t");
    }

    /// `pg_catalog.x` is not the user's `x`. Folding them together would answer a question about a
    /// different table, so a single system reference stops the strip for the whole query.
    #[test]
    fn refuses_when_a_system_schema_is_referenced() {
        for sql in [
            "SELECT a FROM part_1.t JOIN pg_catalog.pg_class c ON c.oid = part_1.t.oid",
            "SELECT a FROM part_1.t JOIN information_schema.columns c ON c.x = part_1.t.y",
            "SELECT a FROM part_1.t JOIN pg_class c ON c.oid = part_1.t.oid",
        ] {
            assert_eq!(unschemad(sql), sql, "should not be rewritten");
        }
    }

    /// `a.orders` and `b.orders` are two tables. Stripping would merge them, turning a join between
    /// two relations into a self-join — a different query, not a different spelling.
    #[test]
    fn refuses_when_one_bare_name_has_two_qualifiers() {
        let sql = "SELECT x FROM a.orders JOIN b.orders ON a.orders.id = b.orders.id";
        assert_eq!(unschemad(sql), sql);
    }

    /// The same qualifier repeated is one table, not a collision.
    #[test]
    fn repeated_identical_qualifiers_are_not_a_collision() {
        assert_eq!(
            unschemad("SELECT x FROM s.orders JOIN s.orders o2 ON s.orders.id = o2.id"),
            "SELECT x FROM orders JOIN orders o2 ON s.orders.id = o2.id"
        );
    }

    /// A bare name beside a qualified one is the same table under two spellings — which is the whole
    /// point of the rewrite, so it must not be read as a collision.
    #[test]
    fn a_bare_name_beside_its_qualified_form_still_strips() {
        assert_eq!(
            unschemad("SELECT x FROM s.orders WHERE id IN (SELECT oid FROM s.lines)"),
            "SELECT x FROM orders WHERE id IN (SELECT oid FROM lines)"
        );
    }

    #[test]
    fn unqualified_queries_are_untouched() {
        assert_eq!(unschemad("SELECT a FROM t JOIN u ON t.i = u.i"), "SELECT a FROM t JOIN u ON t.i = u.i");
    }

    fn inlined(sql: &str) -> String {
        let mut qs = parse_queries(sql, sql);
        inline_ctes(&mut qs);
        qs[0].to_string()
    }

    #[test]
    fn a_binding_becomes_a_derived_table_at_its_use() {
        assert_eq!(
            inlined("WITH c AS (SELECT a FROM t) SELECT * FROM c"),
            "SELECT * FROM (SELECT a FROM t) AS c"
        );
    }

    /// Every use, not the first one. The two copies are the same relation because the frontend has
    /// already refused anything whose value could differ between two evaluations.
    ///
    /// The use's alias also comes through as written, `AS` keyword and all — the rewrite substitutes a
    /// definition, it does not reformat the query around it.
    #[test]
    fn every_use_gets_its_own_copy() {
        assert_eq!(
            inlined("WITH c AS (SELECT a FROM t) SELECT * FROM c x JOIN c y ON x.a = y.a"),
            "SELECT * FROM (SELECT a FROM t) x JOIN (SELECT a FROM t) y ON x.a = y.a"
        );
    }

    /// A use inside a subquery is still a use: the binding is in scope for the whole query.
    #[test]
    fn a_use_in_a_subquery_is_reached() {
        assert_eq!(
            inlined("WITH c AS (SELECT a FROM t) SELECT x FROM u WHERE x IN (SELECT a FROM c)"),
            "SELECT x FROM u WHERE x IN (SELECT a FROM (SELECT a FROM t) AS c)"
        );
    }

    /// A later binding may use an earlier one, so each definition is closed over its predecessors
    /// before it is itself substituted anywhere.
    #[test]
    fn a_binding_that_uses_an_earlier_binding_is_closed_first() {
        assert_eq!(
            inlined("WITH c AS (SELECT a FROM t), d AS (SELECT a FROM c) SELECT * FROM d"),
            "SELECT * FROM (SELECT a FROM (SELECT a FROM t) AS c) AS d"
        );
    }

    /// Innermost first, which is what makes shadowing come out right: the inner binding has consumed
    /// its own use before the outer definition goes looking.
    #[test]
    fn an_inner_binding_shadows_an_outer_one_of_the_same_name() {
        assert_eq!(
            inlined(
                "WITH c AS (SELECT 1 AS z) \
                 SELECT * FROM (WITH c AS (SELECT 2 AS z) SELECT z FROM c) s"
            ),
            "SELECT * FROM (SELECT z FROM (SELECT 2 AS z) AS c) s"
        );
    }

    /// A non-recursive binding is not in scope within its own definition, so `a` there is the base
    /// table and has to survive. This is also the case that would not terminate if the rewrite
    /// re-descended into what it had just substituted in.
    #[test]
    fn a_use_inside_the_definition_of_the_same_name_is_the_base_table() {
        assert_eq!(
            inlined("WITH a AS (SELECT x FROM a) SELECT * FROM a"),
            "SELECT * FROM (SELECT x FROM a) AS a"
        );
    }

    /// The column list renames the output. Dropping it would leave the underlying names to resolve
    /// against, which is a wrong answer rather than a refusal — `WITH c(b, a) AS (SELECT a, b FROM t)`
    /// then makes `SELECT a FROM c` read `t.a` where the query says `t.b`.
    #[test]
    fn a_column_alias_list_is_carried_onto_the_derived_table() {
        assert_eq!(
            inlined("WITH c(b, a) AS (SELECT a, b FROM t) SELECT a FROM c"),
            "SELECT a FROM (SELECT a, b FROM t) AS c (b, a)"
        );
        // A column list at the use site renames again, and wins.
        assert_eq!(
            inlined("WITH c(x, y) AS (SELECT a, b FROM t) SELECT p FROM c AS z (p, q)"),
            "SELECT p FROM (SELECT a, b FROM t) AS z (p, q)"
        );
    }

    /// Unchanged means the guard held, and a surviving `WITH` is what the refusal downstream keys on.
    fn not_inlined(sql: &str) {
        assert_eq!(inlined(sql), sql, "should not be inlined");
    }

    /// A recursive binding denotes a fixpoint, which no finite substitution computes.
    #[test]
    fn recursive_is_left_in_place() {
        not_inlined(
            "WITH RECURSIVE c AS (SELECT a FROM t UNION ALL SELECT a FROM c) SELECT * FROM c",
        );
        // The flag blocks the clause even when nothing in it self-references.
        not_inlined("WITH RECURSIVE c AS (SELECT a FROM t) SELECT * FROM c");
    }

    /// `c(1)` is a table-function call and `s.c` is a table in schema `s`. Neither is a use of the
    /// binding, whatever the name says — the second is why this rewrite runs before [`strip_schema`].
    #[test]
    fn only_a_bare_unqualified_name_is_a_use() {
        assert_eq!(
            inlined("WITH c AS (SELECT a FROM t) SELECT * FROM s.c"),
            "SELECT * FROM s.c"
        );
        assert_eq!(
            inlined("WITH c AS (SELECT a FROM t) SELECT * FROM c(1)"),
            "SELECT * FROM c(1)"
        );
    }

    /// An unused binding just disappears; there is nothing to substitute it into.
    #[test]
    fn an_unused_binding_is_dropped() {
        assert_eq!(inlined("WITH c AS (SELECT a FROM t) SELECT * FROM u"), "SELECT * FROM u");
    }

    fn demoted_sql(sql: &str) -> String {
        let mut st = Parser::parse_sql(&crate::DIALECT, sql).expect("parses");
        demote_operators(&mut st);
        st[0].to_string()
    }

    /// The prefix on the name is not decoration: `infer` reads the return type off it, so `->>`
    /// yielding `text` and `->` yielding `jsonb` have to become differently-prefixed symbols.
    #[test]
    fn json_extraction_carries_its_return_type_in_the_name() {
        assert_eq!(
            demoted_sql("SELECT payload -> 'a' FROM t"),
            "SELECT q_op_jsonx(payload, 'a') FROM t"
        );
        assert_eq!(
            demoted_sql("SELECT payload ->> 'a' FROM t"),
            "SELECT q_str_jsonx(payload, 'a') FROM t"
        );
        assert_eq!(
            demoted_sql("SELECT payload #> '{a,b}' FROM t"),
            "SELECT q_op_jsonx(payload, '{a,b}') FROM t"
        );
        assert_eq!(
            demoted_sql("SELECT payload #>> '{a,b}' FROM t"),
            "SELECT q_str_jsonx(payload, '{a,b}') FROM t"
        );
    }

    /// The call spellings land on the same two symbols, split the same way by return type.
    #[test]
    fn json_extraction_functions_demote_like_their_operators() {
        for n in ["json_extract", "jsonb_extract", "json_extract_path", "jsonb_extract_path"] {
            assert_eq!(
                demoted_sql(&format!("SELECT {n}(payload, 'a') FROM t")),
                "SELECT q_op_jsonx(payload, 'a') FROM t",
                "{n}"
            );
        }
        for n in [
            "json_extract_scalar",
            "jsonb_extract_scalar",
            "json_extract_path_text",
            "jsonb_extract_path_text",
        ] {
            assert_eq!(
                demoted_sql(&format!("SELECT {n}(payload, 'a') FROM t")),
                "SELECT q_str_jsonx(payload, 'a') FROM t",
                "{n}"
            );
        }
    }

    /// The point of the previous test, stated as the property it exists for: the operator and the
    /// function are one operation, so a pair that rewrites between the spellings has to see one
    /// symbol. Two symbols here is not a wrong answer, it is a proof that never lands.
    #[test]
    fn operator_and_function_spellings_agree() {
        assert_eq!(
            demoted_sql("SELECT payload ->> 'a' FROM t"),
            demoted_sql("SELECT json_extract_path_text(payload, 'a') FROM t")
        );
        assert_eq!(
            demoted_sql("SELECT payload -> 'a' FROM t"),
            demoted_sql("SELECT jsonb_extract_path(payload, 'a') FROM t")
        );
    }

    /// The path is variadic, so the arity is carried through rather than fixed at two.
    #[test]
    fn json_extraction_keeps_its_arity() {
        assert_eq!(
            demoted_sql("SELECT json_extract_path(payload, 'a', 'b', 'c') FROM t"),
            "SELECT q_op_jsonx(payload, 'a', 'b', 'c') FROM t"
        );
    }

    /// Every modifier disqualifies the call, and so does a qualified name: none of them is meaningful
    /// on these functions, so one appearing means the name is somebody else's.
    #[test]
    fn only_a_plain_unqualified_call_is_demoted() {
        for sql in [
            // Somebody's own function that happens to share the name.
            "SELECT myschema.json_extract(payload, 'a') FROM t",
            // Not the two-argument extraction.
            "SELECT json_extract(payload) FROM t",
            // Modifiers that make it something other than a plain scalar call.
            "SELECT json_extract(DISTINCT payload, 'a') FROM t",
            "SELECT json_extract(payload, 'a') OVER () FROM t",
        ] {
            let out = demoted_sql(sql);
            assert!(!out.contains("jsonx"), "should have been left alone: {sql} -> {out}");
        }
    }

    /// Post-order applies across the two spellings too, not just within one.
    #[test]
    fn a_function_extraction_nests_with_an_operator_one() {
        assert_eq!(
            demoted_sql("SELECT json_extract_path_text(payload -> 'a', 'b') FROM t"),
            "SELECT q_str_jsonx(q_op_jsonx(payload, 'a'), 'b') FROM t"
        );
    }

    /// Bottom-up: an operator inside another operator's operand is a call before the outer one is
    /// rewritten, so a chain collapses in one pass.
    #[test]
    fn a_chain_of_extractions_collapses() {
        assert_eq!(
            demoted_sql("SELECT payload -> 'a' ->> 'b' FROM t"),
            "SELECT q_str_jsonx(q_op_jsonx(payload, 'a'), 'b') FROM t"
        );
    }

    /// The one test here that is about the *parser* rather than about this module: it pins the
    /// precedence [`DIALECT`][crate::DIALECT] buys.
    ///
    /// sqlparser's `GenericDialect` gives the Arrow family no precedence at all — they are absent from
    /// its token table and fall through to `prec_unknown()`, which is 0 — so it reads
    /// `payload ->> 'k' = 'v'` as `payload ->> ('k' = 'v')`, comparing the key to the value and passing
    /// the boolean in as the key. `PostgreSqlDialect` gives them `PG_OTHER_PREC`, above `=`.
    ///
    /// Demotion cannot detect this and would not fail on it: it would faithfully build
    /// `q_str_jsonx(payload, 'k' = 'v')`, the prover would prove something true about that, and the
    /// verdict would be reported for a query nobody wrote. So the guard is here, one level below the
    /// rewrite that depends on it, asserting the operand shape rather than the dialect's name.
    #[test]
    fn extraction_binds_tighter_than_the_comparison_around_it() {
        assert_eq!(
            demoted_sql("SELECT 1 FROM t WHERE payload ->> 'k' = 'v'"),
            "SELECT 1 FROM t WHERE q_str_jsonx(payload, 'k') = 'v'"
        );
        // Both spellings of the path form, since they are separate tokens with separately-missing
        // entries in the table.
        assert_eq!(
            demoted_sql("SELECT 1 FROM t WHERE payload #>> '{a,b}' <> 'v'"),
            "SELECT 1 FROM t WHERE q_str_jsonx(payload, '{a,b}') <> 'v'"
        );
        // The jsonb-returning arrows sit above `=` too, and above `IS NULL`.
        assert_eq!(
            demoted_sql("SELECT 1 FROM t WHERE payload -> 'k' IS NULL"),
            "SELECT 1 FROM t WHERE q_op_jsonx(payload, 'k') IS NULL"
        );
        // Above the pattern-match group as well, which is a separate entry in the table from `=`.
        assert_eq!(
            demoted_sql("SELECT 1 FROM t WHERE payload ->> 'k' LIKE 'v%'"),
            "SELECT 1 FROM t WHERE q_str_jsonx(payload, 'k') LIKE 'v%'"
        );
        // Not a precedence case — the parentheses settle the grouping in any dialect — but it pins that
        // demotion rewrites *under* a cast rather than around it, which is what lets `casts` see an
        // operand whose type it can name.
        assert_eq!(
            demoted_sql("SELECT 1 FROM t WHERE (payload ->> 'k')::int > 3"),
            "SELECT 1 FROM t WHERE (q_str_jsonx(payload, 'k'))::INT > 3"
        );
    }

    /// `a @> b` and `b <@ a` are one relation read two ways, so they share a symbol with the operands
    /// swapped — which is what lets the prover see the two spellings as equal rather than as two
    /// unrelated calls.
    #[test]
    fn containment_shares_one_symbol_in_both_directions() {
        assert_eq!(
            demoted_sql("SELECT 1 FROM t WHERE tags @> other"),
            "SELECT 1 FROM t WHERE q_bool_contains(tags, other)"
        );
        assert_eq!(
            demoted_sql("SELECT 1 FROM t WHERE other <@ tags"),
            "SELECT 1 FROM t WHERE q_bool_contains(tags, other)"
        );
    }

    /// `x !~ p` is `NOT (x ~ p)` in Postgres, nulls included, so the negated form gets the same symbol
    /// under a `NOT` rather than a symbol of its own.
    #[test]
    fn a_negated_regex_match_is_the_negation_of_the_match() {
        assert_eq!(
            demoted_sql("SELECT 1 FROM t WHERE name ~ '^a'"),
            "SELECT 1 FROM t WHERE q_bool_rematch(name, '^a')"
        );
        assert_eq!(
            demoted_sql("SELECT 1 FROM t WHERE name !~ '^a'"),
            "SELECT 1 FROM t WHERE NOT q_bool_rematch(name, '^a')"
        );
        // The case-insensitive pair is a second symbol, not a flag on the first: `~` and `~*` are
        // different relations.
        assert_eq!(
            demoted_sql("SELECT 1 FROM t WHERE name !~* '^a'"),
            "SELECT 1 FROM t WHERE NOT q_bool_reimatch(name, '^a')"
        );
    }

    /// `AT TIME ZONE` is a two-argument function wearing keyword clothes.
    #[test]
    fn at_time_zone_becomes_a_two_argument_call() {
        assert_eq!(
            demoted_sql("SELECT ts AT TIME ZONE 'utc' FROM t"),
            "SELECT q_op_attz(ts, 'utc') FROM t"
        );
        // Nested inside another demotion, to check the post-order holds across the two kinds.
        assert_eq!(
            demoted_sql("SELECT 1 FROM t WHERE (payload ->> 'k') AT TIME ZONE z IS NULL"),
            "SELECT 1 FROM t WHERE q_op_attz((q_str_jsonx(payload, 'k')), z) IS NULL"
        );
    }

    /// The three spellings Postgres makes one value share one symbol, so a pair that swaps one for
    /// another still proves.
    #[test]
    fn the_synonymous_clocks_share_a_symbol() {
        for sql in ["SELECT now() FROM t", "SELECT CURRENT_TIMESTAMP FROM t", "SELECT transaction_timestamp() FROM t"] {
            assert_eq!(demoted_sql(sql), "SELECT q_op_now(0) FROM t", "from {sql}");
        }
    }

    /// ...and the ones that are *not* the same value do not, which is where the preprocessor's single
    /// `q_int_now` would assert that a date equals a timestamp.
    #[test]
    fn clocks_that_differ_get_different_symbols() {
        assert_eq!(demoted_sql("SELECT CURRENT_DATE FROM t"), "SELECT q_op_curdate(0) FROM t");
        assert_eq!(demoted_sql("SELECT LOCALTIMESTAMP FROM t"), "SELECT q_op_localts(0) FROM t");
        assert_eq!(
            demoted_sql("SELECT statement_timestamp() FROM t"),
            "SELECT q_op_stmt_ts(0) FROM t"
        );
    }

    /// The clock rule keys on a bare nullary name, so it cannot capture a user function that happens
    /// to be spelled the same.
    #[test]
    fn only_a_bare_nullary_clock_is_the_constant() {
        for sql in [
            // Somebody's own `now`, taking an argument.
            "SELECT now(a) FROM t",
            // ...and somebody's own schema-qualified one.
            "SELECT s.now() FROM t",
        ] {
            assert_eq!(demoted_sql(sql), sql, "should not be demoted");
        }
    }

    /// A moving clock is not demoted here at all — `lower` refuses it, and quietly turning it into a
    /// constant would be the assertion that guard exists to prevent.
    #[test]
    fn a_clock_that_moves_during_the_query_is_not_a_constant() {
        for sql in ["SELECT clock_timestamp() FROM t", "SELECT random() FROM t"] {
            assert_eq!(demoted_sql(sql), sql, "should not be demoted");
        }
    }

    /// Everything the prover models concretely has to stay concrete: demoting `=` or `+` to an opaque
    /// call would throw away the arithmetic the proofs actually run on.
    #[test]
    fn ordinary_operators_are_left_alone() {
        for sql in [
            "SELECT 1 FROM t WHERE a = b",
            "SELECT a + b FROM t",
            "SELECT 1 FROM t WHERE a AND b",
            "SELECT a || b FROM t",
            // Bitwise-or is deliberately still refused: the return type would be a guess.
            "SELECT a | b FROM t",
        ] {
            assert_eq!(demoted_sql(sql), sql, "should not be demoted");
        }
    }

    // -----------------------------------------------------------------------
    // distribute_array_casts
    // -----------------------------------------------------------------------

    /// Round-trip one statement through the `ARRAY[..]::T[]` -> `ARRAY[..::T]` rewrite.
    fn distributed(sql: &str) -> String {
        let mut st = Parser::parse_sql(&crate::DIALECT, sql).expect("parses");
        distribute_array_casts(&mut st);
        st[0].to_string()
    }

    /// The negative form: a guard held and the statement is untouched.
    fn not_distributed(sql: &str) {
        assert_eq!(distributed(sql), sql, "should not be rewritten");
    }

    /// The `CastKind` rides along, so a `::` cast distributes into `::` casts and a `CAST(.. AS ..)`
    /// into `CAST`s. That is not cosmetic: `casts::decide` keys a `qcast` symbol on the operand's
    /// rendered text, so a pass that silently re-spelled every cast it touched would split symbols
    /// that should share one.
    #[test]
    fn pushes_an_array_cast_onto_the_elements() {
        assert_eq!(
            distributed("SELECT x FROM t WHERE x = ANY(ARRAY[$1, $2]::BIGINT[])"),
            "SELECT x FROM t WHERE x = ANY(ARRAY[$1::BIGINT, $2::BIGINT])"
        );
        // One element and the `<> ALL` spelling, which lowers off the same arm.
        assert_eq!(
            distributed("SELECT x FROM t WHERE x <> ALL(ARRAY[$1]::TEXT[])"),
            "SELECT x FROM t WHERE x <> ALL(ARRAY[$1::TEXT])"
        );
        // A sized target constrains the array, not the element, so the element cast is the same.
        assert_eq!(
            distributed("SELECT ARRAY[$1]::INT[3] FROM t"),
            "SELECT ARRAY[$1::INT] FROM t"
        );
        // The keyword spelling, and a non-placeholder element: nothing about the rewrite is
        // specific to `$N`.
        assert_eq!(
            distributed("SELECT CAST(ARRAY['a', b] AS TEXT[]) FROM t"),
            "SELECT ARRAY[CAST('a' AS TEXT), CAST(b AS TEXT)] FROM t"
        );
        // `TRY_CAST` distributes as itself: it is the same function with a different failure mode,
        // and that mode is per-element on both sides.
        assert_eq!(
            distributed("SELECT TRY_CAST(ARRAY[$1] AS INT[]) FROM t"),
            "SELECT ARRAY[TRY_CAST($1 AS INT)] FROM t"
        );
    }

    /// A cast to a scalar type is not an element-wise cast, and distributing it would assert
    /// something the query does not say.
    #[test]
    fn a_non_array_target_is_left_alone() {
        not_distributed("SELECT CAST(ARRAY[$1, $2] AS TEXT) FROM t");
    }

    /// `ARRAY[]::T[]` has nowhere to put `T`, so distributing would discard the only statement of
    /// the element type. It keeps `casts`'s refusal instead.
    #[test]
    fn an_empty_literal_is_left_alone() {
        not_distributed("SELECT CAST(ARRAY[] AS TEXT[]) FROM t");
    }

    /// Not an array literal at all: a cast over a *column* of array type is untouched, since there
    /// are no elements in the tree to push onto.
    #[test]
    fn a_cast_over_something_that_is_not_an_array_literal_is_left_alone() {
        not_distributed("SELECT CAST(tags AS TEXT[]) FROM t");
        not_distributed("SELECT CAST($1 AS TEXT[]) FROM t");
    }

    /// The visitor is post-order, so the inner cast distributes first. The outer one then pushes
    /// `INT[]` onto an element that is itself an array — leaving a cast over an array literal one
    /// level down, which `casts` still refuses. Half-rewritten is not reachable: the refusal is on
    /// the shape, not on whether this pass touched the node.
    #[test]
    fn nesting_distributes_from_the_inside_out() {
        assert_eq!(
            distributed("SELECT CAST(ARRAY[CAST(ARRAY[$1] AS INT[])] AS INT[][]) FROM t"),
            "SELECT ARRAY[CAST(ARRAY[CAST($1 AS INT)] AS INT[])] FROM t"
        );
    }

    /// Idempotent, which is what makes the pass safe to run in both copies of the chain
    /// (`parse_input` and `normalized_pair`): applying it twice is applying it once.
    #[test]
    fn distributing_twice_changes_nothing() {
        let once = distributed("SELECT x FROM t WHERE x = ANY(ARRAY[$1, $2]::BIGINT[])");
        assert_eq!(distributed(&once), once);
    }
}
