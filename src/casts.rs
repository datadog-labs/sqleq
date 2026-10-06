// Unless explicitly stated otherwise all files in this repository are licensed under the
// Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/).
// Copyright 2026-Present Datadog, Inc.

//! What to do with a cast the query writes down.
//!
//! [`infer`](crate::infer) decides what type everything *has*. This module decides what to do with
//! the places the query states a type outright — every `CAST(x AS T)` / `x::T` — which is the last
//! thing standing between the SQL of an input pair and the IR the prover reads.
//!
//! ## The five rules
//!
//! | | operand | becomes | why it is faithful |
//! |---|---|---|---|
//! | 1 | `$N` | `$N` | the parameter *is* an uninterpreted constant of the target type, so the cast is the identity on it |
//! | 2/3 | a literal or `NULL` | `CAST(lit AS T′)` | a real cast survives; `T′` is only the prover's spelling of `T` |
//! | 4 | anything already of type `T`, target unqualified | the operand | a no-op cast |
//! | 5a | `INTEGER` → `REAL` | `CAST(x AS NUMERIC)` | a widening the prover models natively |
//! | 5b | any other real conversion | `qcastK(x)` | an uninterpreted function, shared across both sides |
//!
//! Rule 4's *unqualified* condition is load-bearing: `x::varchar` over a VARCHAR column is a no-op,
//! but `x::varchar(8)` is a truncation and must not be dropped. [`map_type_name`] reports the
//! qualifier separately for exactly this.
//!
//! ## Why rule 5b cannot license a false proof
//!
//! `qcastK` is a fresh uninterpreted function symbol `f : S → T`, standing in for the real
//! conversion `g`. The prover quantifies over every interpretation of `f`, and `f = g` is one of
//! them — so a proof that holds for all `f` holds for `g`. That argument does not depend on which
//! occurrences share a symbol, which means **both sharing and splitting are sound** and the dedup
//! key only trades completeness: sharing lets the two sides cancel a conversion against each other,
//! splitting stops them. The preprocessor keys on `(target type, operand text)`, so this does too.
//!
//! One consequence worth naming, because it is a quirk rather than a design: the key omits the
//! *argument* type, so two operands that render identically but were inferred differently collide,
//! and the declaration records whichever arrived first. Faithful to the original, and harmless — a
//! wrong argument sort makes the symbol apply to a different domain, not the conversion unsound.
//!
//! ## Mutating a tree that identity maps point into
//!
//! Inference keys everything on [`nid`] — a node's address, standing in for Python's `id(node)`.
//! Python can rewrite freely because its objects never move; Rust's do. Two properties keep the keys
//! valid here:
//!
//! * **Decide first, then apply.** [`decide`] reads the untouched tree and records one [`Decision`]
//!   per cast. Nothing in that pass depends on another cast having been rewritten already — the
//!   preprocessor's decisions are order-independent too — so splitting them costs nothing.
//! * **Apply bottom-up, in place.** [`Apply`] fires on `post_visit_expr`, so a node is rewritten
//!   only after its descendants, and it *overwrites* `*e` rather than replacing the parent's `Box`.
//!   An ancestor therefore never moves while its descendants are being rewritten, and the
//!   decision keys stay live for the whole pass.
//!
//! What does move is a hoisted operand (rules 1 and 4), from wherever it lived into the cast's slot.
//! Only that one node changes address — its own children sit behind `Box`/`Vec` indirections whose
//! heap buffers do not move — so re-keying the single entry restores the invariant. [`Apply`]
//! collects those and [`rewrite_casts`] applies them.
//!
//! Rule 5b is the mirror image: the preprocessor wraps a `.copy()` of the operand, which in Python
//! means every `id()` inside it becomes fresh and every map built before the rewrite stops
//! resolving there. Moving the subtree in Rust invalidates only its root. The remaining addresses
//! are dropped explicitly so that the port sees the same thing the original does.
//!
//! ### Cast over cast
//!
//! A cast can be the direct operand of another cast, so both invariants have to hold for a chain.
//! They do, and one consequence is worth stating because it is load-bearing rather than incidental:
//!
//! * Bottom-up means the inner cast is rewritten into its own slot before the outer is visited, so
//!   the outer still finds an `Expr::Cast` there and re-reads the rewritten operand. The outer's
//!   *decision* was taken from the inner's declared `data_type`, not from the inner's decision, so
//!   the two passes stay independent.
//! * `let mut op = *expr` in [`Apply`] does free a `Box` mid-traversal, and a later allocation can
//!   land on that address. It can never resurrect a live key: bottom-up guarantees the freed box
//!   belonged to a *descendant*, whose decision was already consumed.
//! * Two chained hoists push `(inner operand, inner slot)` and then `(inner slot, outer slot)`.
//!   [`rewrite_casts`] replays `remap` in push order, so the entry walks the chain to its final
//!   home. **The order is the correctness argument**, not a coincidence of the data structure.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::ops::ControlFlow;

use sqlparser::ast::{
    DataType, ExactNumberInfo, Expr, Function, FunctionArg, FunctionArgExpr, FunctionArgumentList,
    FunctionArguments, Ident, ObjectName, ObjectNamePart, Query, TimezoneInfo, Value as SqlValue, VisitMut,
    VisitorMut,
};

use crate::catalog::{obj_name, FnDecl};
use crate::error::{unsupported, FrontendError, Result};
use crate::infer::{
    builtin_rtype, canon_type_name, is_agg_name, map_type_name, nid, sweep, Atom, Inferred, Ty, Uf,
};

/// A shared uninterpreted conversion: rule 5b's `qcastK`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct QCast {
    /// `qcast0`, `qcast1`, … — numbered in the order the rewrite first needed them.
    pub name: String,
    /// The operand's inferred type, as declared.
    pub arg: Ty,
    /// The cast's target type.
    pub ret: Ty,
}

/// How many casts each rule that *deletes* one accounted for.
///
/// Only the deleting rules are counted, because the count exists to explain the differential: a
/// deleted cast is evidence the emitted case no longer states, so anything re-inferring from that
/// case is working from strictly less than the preprocessor had. Mirrors `dropped_casts`.
#[derive(Default, Clone, Copy, Debug, PartialEq, Eq)]
pub struct Dropped {
    pub param: usize,
    pub identity: usize,
    pub qcast: usize,
}

/// What the rewrite produced.
#[derive(Default, Debug)]
pub struct Rewrite {
    pub qcasts: Vec<QCast>,
    pub dropped: Dropped,
}

/// What to do with one cast. Recorded against the cast node's [`nid`] by [`decide`].
#[derive(Clone, Debug)]
enum Decision {
    /// Rules 1 and 4: the cast goes and its operand takes its place. Carries the operand's address
    /// before the move, so the identity maps can be re-keyed after it.
    Hoist { operand: usize },
    /// Rules 2/3 and 5a: a real cast survives, retargeted to the prover's spelling of the type.
    Retarget(Ty),
    /// Rule 5b: wrap the operand in a shared symbol. Carries the index into [`Rewrite::qcasts`] and
    /// every address inside the operand, which the preprocessor's `.copy()` invalidates.
    Wrap { idx: usize, copied: Vec<usize> },
}

/// The prover's spelling of a type, as a `DataType` [`crate::types::map_type`] will read back.
fn data_type(t: Ty) -> DataType {
    match t {
        Ty::Int => DataType::Integer(None),
        // `numeric`, not `double precision`: a float is opaque to `map_type`, and `Ty::Real` is exact.
        Ty::Real => DataType::Numeric(ExactNumberInfo::None),
        Ty::Str => DataType::Varchar(None),
        Ty::Bool => DataType::Boolean,
        Ty::Date => DataType::Date,
        Ty::Time => DataType::Time(None, TimezoneInfo::None),
        Ty::Timestamp => DataType::Timestamp(None, TimezoneInfo::None),
        Ty::TimestampTz => DataType::Timestamp(None, TimezoneInfo::WithTimeZone),
        Ty::Interval => DataType::Interval { fields: None, precision: None },
        Ty::Opaque => DataType::Varbinary(None),
    }
}

/// Whether `t` is one of the temporal types, which never cross into each other or into anything else
/// without a named conversion (see `types.rs`).
fn is_temporal(t: Ty) -> bool {
    matches!(t, Ty::Date | Ty::Time | Ty::Timestamp | Ty::TimestampTz | Ty::Interval)
}

/// `name(arg)` — the shape both `qcastK` and `qpN` take.
fn call(name: &str, arg: Expr) -> Expr {
    Expr::Function(Function {
        name: ObjectName(vec![ObjectNamePart::Identifier(Ident::new(name))]),
        uses_odbc_syntax: false,
        parameters: FunctionArguments::None,
        args: FunctionArguments::List(FunctionArgumentList {
            duplicate_treatment: None,
            args: vec![FunctionArg::Unnamed(FunctionArgExpr::Expr(arg))],
            clauses: Vec::new(),
        }),
        filter: None,
        null_treatment: None,
        over: None,
        within_group: Vec::new(),
    })
}

/// `$N` written as a placeholder, which is what rule 1 keys on.
///
/// Narrower than [`crate::infer::param_index`], which also accepts the substituted `qpN(0)` form:
/// the preprocessor's rule 1 tests for `exp.Parameter` specifically, and a `qpN(0)` reaching here
/// would be a call, taking the rule-4/5 path. Keeping the distinction keeps the port honest on
/// already-prepared input.
fn placeholder_index(e: &Expr) -> Option<u32> {
    match e {
        Expr::Value(v) => match &v.value {
            SqlValue::Placeholder(p) => p.strip_prefix('$')?.parse().ok(),
            _ => None,
        },
        _ => None,
    }
}

/// Strip the parentheses off `(expr)::T` the way the preprocessor does before it classifies the
/// operand — so `($1)::int` is rule 1, not "cast over unsupported operand".
pub(crate) fn unwrap_nested(mut e: &Expr) -> &Expr {
    while let Expr::Nested(inner) = e {
        e = inner;
    }
    e
}

/// Every address in a subtree, for the copy semantics of rule 5b.
fn subtree_ids(e: &Expr) -> Vec<usize> {
    let mut out = vec![nid(e)];
    let _ = sqlparser::ast::visit_expressions(e, |x| {
        out.push(nid(x));
        ControlFlow::<()>::Continue(())
    });
    out
}

/// A literal or `NULL` — the operands of rules 2/3.
///
/// `Expr::Value` also covers placeholders, which rule 1 has already claimed by the time this runs.
fn is_literal(e: &Expr) -> bool {
    matches!(e, Expr::Value(v) if !matches!(v.value, SqlValue::Placeholder(_)))
}

/// The dedup key's view of a cast target: two casts share a `qcast` symbol exactly when this agrees.
///
/// Sharing a symbol is what lets an untouched conversion cancel between the two sides of a pair, and
/// it is also the one way this rewrite could prove something false -- two casts that compute
/// different things under one name. So the rule is: collapse a spelling only where the spelling is
/// all that differs.
///
/// * An unqualified mappable target collapses to its [`Ty`], so `x::text` and `x::varchar` keep
///   sharing one symbol and a pair that merely respells a cast still cancels.
/// * A qualified one keeps its own spelling. `varchar(8)` and `varchar(4)` are different truncations
///   of the same column, and the [`Ty`] alone cannot tell them apart.
/// * An unmappable one keeps its own spelling too -- it has no [`Ty`] to collapse to, and every
///   unmappable target shares [`Ty::Opaque`], so collapsing would make `x::jsonb` and `x::uuid[]` one
///   function. Z3 identifies an uninterpreted function by name and sorts, and both of those carry as
///   VARBINARY, so the name is the only thing keeping them apart.
fn canon_target(txt: &str, target: Option<Ty>, qualified: bool) -> String {
    match target {
        Some(t) if !qualified => t.sql().to_string(),
        _ => canon_type_name(txt),
    }
}

/// The dedup key's view of a cast operand: its text, with unquoted names folded to lower case.
///
/// The text stands in for what the operand computes, its type included, and Postgres folds an
/// unquoted name before it reads it, so `SUM(x)` and `sum(X)` are one call and their casts one
/// function. Literals and quoted names are compared as written.
fn operand_key(op: &Expr) -> String {
    let mut e = op.clone();
    let _ = e.visit(&mut FoldNames);
    e.to_string()
}

struct FoldNames;

impl VisitorMut for FoldNames {
    type Break = ();

    fn pre_visit_expr(&mut self, e: &mut Expr) -> ControlFlow<Self::Break> {
        let fold = |id: &mut Ident| {
            if id.quote_style.is_none() {
                id.value = id.value.to_lowercase();
            }
        };
        match e {
            Expr::Identifier(id) => fold(id),
            Expr::CompoundIdentifier(ids) => ids.iter_mut().for_each(fold),
            Expr::Function(f) => f.name.0.iter_mut().for_each(|p| {
                if let ObjectNamePart::Identifier(id) = p {
                    fold(id)
                }
            }),
            _ => {}
        }
        ControlFlow::Continue(())
    }
}

/// Read the untouched tree and record what should happen to each cast.
fn decide(
    queries: &[Query],
    inf: &mut Inferred,
    dec: &mut HashMap<usize, Decision>,
    rw: &mut Rewrite,
) -> Result<()> {
    // (canonical target, operand text) -> index into `rw.qcasts`. This key decides which casts are
    // *the same function*, so everything the target says that changes what the cast computes has to
    // be in it -- see `canon_target`.
    let mut seen: HashMap<(String, String), usize> = HashMap::new();
    for q in queries {
        sweep(q, |e| {
            let Expr::Cast { expr, data_type: dt, .. } = e else { return Ok(()) };
            let txt = dt.to_string();
            // A `citext` or `char(n)` value is refused wherever it appears (`types::UNFAITHFUL`). A
            // `qcast` would carry it as an opaque value, whose `=` is the prover's equality.
            if let Some(u) = crate::types::unfaithful_type(&txt) {
                return Err(crate::types::unfaithful_refusal(u));
            }
            let (target, qualified) = map_type_name(&txt);
            // A target with no `Ty` is still a deterministic function of its operand; what is missing
            // is an interpretation, not a value. `Ty::Opaque` carries it -- VARBINARY, which the
            // prover gives an equality-only sort -- and rule 5b below wraps the cast in a symbol
            // named after the target. See [`map_type_name`] for why inference does not do the same.
            let tq = target.unwrap_or(Ty::Opaque);
            let op = unwrap_nested(expr);

            // A typmod is a computation, not a type: `$1::varchar(2)` truncates and
            // `$1::timestamp(0)` rounds, so only an unqualified cast over a parameter is the
            // parameter's type and nothing more. A qualified one takes rule 5b, keyed on the
            // qualified spelling.
            if placeholder_index(op).is_some() && !qualified {
                rw.dropped.param += 1;
                dec.insert(nid(e), Decision::Hoist { operand: nid(op) });
                return Ok(());
            }
            // The same holds over a literal: `'abc'::varchar(2)` is `'ab'`, and retargeting it to the
            // bare type would make it a cast between equal types, which is the identity.
            if is_literal(op) && !qualified {
                dec.insert(nid(e), Decision::Retarget(tq));
                return Ok(());
            }

            // Only rules 4 and 5a consult the operand's type; rule 5b does not. `QCast::arg` is
            // recorded for the record's sake and never reaches a declaration — `emit_decls` writes
            // `decl(q.ret, ..)` — so an operand nobody can type is not a reason to refuse. It is a
            // reason to take 5b, and `Ty::Opaque` gets it there on its own:
            //
            // * rule 4 is gated on `target.is_some() && cqt == tq`, and [`map_type_name`] returns
            //   `Some` only for Int/Real/Str/Bool — never `Some(Ty::Opaque)` — so the two cannot
            //   both hold;
            // * rule 5a needs `cqt == Ty::Int`.
            //
            // Both gates are therefore unreachable, and 5b is the conservative branch: it neither
            // drops the cast nor retargets it, it names it. Same shape as the argument for an
            // unmappable *target* above.
            let cqt = match op {
                Expr::Identifier(_) | Expr::CompoundIdentifier(_) => {
                    match inf.col.get(&nid(op)).cloned() {
                        Some(a) => inf.uf.get_type(&a).unwrap_or(Ty::Opaque),
                        None => Ty::Opaque,
                    }
                }
                // A cast states its own result type, which makes a nested cast the *most* precise
                // operand there is — better evidence than inference, and read from the untouched
                // tree, so `decide` stays order-independent. `kind` is ignored here as it is for the
                // outer cast: `TRY_CAST(x AS int)` still has type `int`, it just yields NULL instead
                // of erroring.
                Expr::Cast { data_type, .. } => {
                    map_type_name(&data_type.to_string()).0.unwrap_or(Ty::Opaque)
                }
                // The old `if is_opaque_call(op)` guard excluded exactly the *modelled* functions —
                // the ones whose return type `fn_ret` knows — and bought nothing: both sides of the
                // guard ended in the same `unwrap_or(Ty::Opaque)`.
                Expr::Function(f) => builtin_rtype(&obj_name(&f.name))
                    .or_else(|| inf.fn_ret.get(&nid(op)).copied())
                    .unwrap_or(Ty::Opaque),
                // The two kinds still worth refusing. `lower.rs`'s `lower_expr` has no arm for a
                // tuple, so wrapping one in a `qcast` would move the refusal rather than lift it;
                // and a `qcast` around an array literal would cost the exact OR-expansion
                // `lower_quantified` gives one under `= ANY`.
                other @ (Expr::Array(_) | Expr::Tuple(_)) => {
                    return Err(unsupported(format!(
                        "cast over unsupported operand {}",
                        operand_kind(other)
                    )))
                }
                _ => Ty::Opaque,
            };

            // `target.is_some()` gates the identity rule, and it must: with an unmappable target
            // both sides of `cqt == tq` can be `Ty::Opaque` -- an unknown column cast to `jsonb` --
            // and dropping the cast then equates `x::jsonb` with a bare `x`. Not knowing what a cast
            // computes is the opposite of knowing it computes nothing.
            if target.is_some() && !qualified && cqt == tq {
                rw.dropped.identity += 1;
                dec.insert(nid(e), Decision::Hoist { operand: nid(op) });
            } else if cqt == Ty::Int && tq == Ty::Real {
                dec.insert(nid(e), Decision::Retarget(Ty::Real));
            } else if target.is_some()
                && !qualified
                && cqt != Ty::Opaque
                && (is_temporal(cqt) || is_temporal(tq))
            {
                // A crossing between two known types, one of them temporal: the cast stays, and the
                // lowering turns it into the conversion named after both types
                // (`types::lower_cast`). Named after the types rather than after this operand's text,
                // because a conversion is a function of its source type, its target and its value,
                // and the source type is known here -- so `ts::date` on both sides of a pair is one
                // function, where a `qcast` keyed on each operand's text would split it. An operand
                // of unknown type still takes rule 5b below.
                dec.insert(nid(e), Decision::Retarget(tq));
            } else {
                rw.dropped.qcast += 1;
                // Keyed on the operand's *pre-rewrite* text, so `(x::varchar)::varchar(8)` and
                // `x::varchar(8)` get separate symbols even though the inner cast is about to be
                // hoisted away and the two render identically afterwards. Splitting is sound (see
                // the module docs); it costs a cancellation, which is the same trade the key
                // already makes everywhere else.
                let key = (canon_target(&txt, target, qualified), operand_key(op));
                let idx = *seen.entry(key).or_insert_with(|| {
                    rw.qcasts.push(QCast {
                        name: format!("qcast{}", rw.qcasts.len()),
                        arg: cqt,
                        ret: tq,
                    });
                    rw.qcasts.len() - 1
                });
                dec.insert(nid(e), Decision::Wrap { idx, copied: subtree_ids(op) });
            }
            Ok(())
        })?;
    }
    Ok(())
}

/// A name for the operand shape in the refusal message. The preprocessor prints sqlglot's node
/// class; this prints the nearest sqlparser equivalent, since the two taxonomies do not line up.
fn operand_kind(e: &Expr) -> &'static str {
    match e {
        Expr::Cast { .. } => "Cast",
        Expr::Function(_) => "Func",
        Expr::BinaryOp { .. } | Expr::UnaryOp { .. } => "Arithmetic",
        Expr::Case { .. } => "Case",
        Expr::Subquery(_) => "Subquery",
        Expr::Tuple(_) => "Tuple",
        Expr::Array(_) => "Array",
        _ => "Expr",
    }
}

/// Apply the decisions bottom-up. See the module docs for why the keys survive.
struct Apply<'a> {
    dec: &'a HashMap<usize, Decision>,
    qcasts: &'a [QCast],
    /// `(old address, new address)` for each hoisted operand.
    remap: Vec<(usize, usize)>,
    /// Addresses the preprocessor's `.copy()` would have invalidated.
    dead: Vec<usize>,
}

impl VisitorMut for Apply<'_> {
    type Break = FrontendError;

    fn post_visit_expr(&mut self, e: &mut Expr) -> ControlFlow<Self::Break> {
        let Some(d) = self.dec.get(&nid(e)).cloned() else { return ControlFlow::Continue(()) };
        // Only cast nodes were given decisions, and nothing has overwritten this one: its
        // descendants were rewritten in place, which does not disturb the node itself.
        if !matches!(e, Expr::Cast { .. }) {
            return ControlFlow::Continue(());
        }
        if let Decision::Retarget(t) = d {
            if let Expr::Cast { data_type: dt, .. } = e {
                *dt = data_type(t);
            }
            return ControlFlow::Continue(());
        }
        let here = nid(e);
        let Expr::Cast { expr, .. } = std::mem::replace(e, Expr::Identifier(Ident::new(""))) else {
            unreachable!("checked just above")
        };
        let mut op = *expr;
        while let Expr::Nested(inner) = op {
            op = *inner;
        }
        match d {
            Decision::Hoist { operand } => {
                *e = op;
                self.remap.push((operand, here));
            }
            Decision::Wrap { idx, copied } => {
                *e = call(&self.qcasts[idx].name, op);
                self.dead.extend(copied);
            }
            Decision::Retarget(_) => unreachable!("handled above"),
        }
        ControlFlow::Continue(())
    }
}

/// Rewrite every cast in `queries`, updating `inf`'s identity maps to match.
pub fn rewrite_casts(queries: &mut [Query], inf: &mut Inferred) -> Result<Rewrite> {
    let mut dec = HashMap::new();
    let mut rw = Rewrite::default();
    decide(queries, inf, &mut dec, &mut rw)?;
    if dec.is_empty() {
        return Ok(rw);
    }
    let mut ap = Apply { dec: &dec, qcasts: &rw.qcasts, remap: Vec::new(), dead: Vec::new() };
    for q in queries.iter_mut() {
        if let ControlFlow::Break(err) = q.visit(&mut ap) {
            return Err(err);
        }
    }
    let (remap, dead) = (ap.remap, ap.dead);
    for (old, new) in remap {
        if let Some(a) = inf.col.remove(&old) {
            inf.col.insert(new, a);
        }
        if let Some(t) = inf.fn_ret.remove(&old) {
            inf.fn_ret.insert(new, t);
        }
    }
    let dead: HashSet<usize> = dead.into_iter().collect();
    inf.col.retain(|k, _| !dead.contains(k));
    inf.fn_ret.retain(|k, _| !dead.contains(k));
    Ok(rw)
}

/// Replace every `$N` with `qpN(0)`, the shared uninterpreted constant the prover reads.
///
/// A parameter has one value per execution, the same value everywhere it appears and the same on
/// both sides of the pair. A nullary uninterpreted symbol says exactly that; `0` is a filler
/// argument because the prover's declaration DSL has no nullary form.
pub fn substitute_params(queries: &mut [Query], inf: &mut Inferred) -> Result<()> {
    struct Sub {
        remap: Vec<(usize, usize)>,
    }
    impl VisitorMut for Sub {
        type Break = FrontendError;
        fn post_visit_expr(&mut self, e: &mut Expr) -> ControlFlow<Self::Break> {
            let Some(n) = placeholder_index(e) else { return ControlFlow::Continue(()) };
            let here = nid(e);
            *e = call(&format!("qp{n}"), Expr::Value(SqlValue::Number("0".into(), false).into()));
            self.remap.push((here, here));
            ControlFlow::Continue(())
        }
    }
    let mut sub = Sub { remap: Vec::new() };
    for q in queries.iter_mut() {
        if let ControlFlow::Break(err) = q.visit(&mut sub) {
            return Err(err);
        }
    }
    // A placeholder is a leaf and carries no attribution of its own, so nothing needs re-keying —
    // but the node that replaced it is a *call*, and leaving a stale `fn_ret` entry at that address
    // would hand `qpN` an invented return type during signature synthesis.
    for (_, at) in sub.remap {
        inf.fn_ret.remove(&at);
        inf.col.remove(&at);
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Declaration synthesis
// ---------------------------------------------------------------------------

/// The pieces of [`Inferred`] a signature needs, split out so a walk can borrow the tree while the
/// typing borrows the evidence.
struct Types<'a> {
    col: &'a HashMap<usize, Atom>,
    fn_ret: &'a HashMap<usize, Ty>,
    params: &'a BTreeMap<u32, Ty>,
    qcast_ret: HashMap<String, Ty>,
    uf: &'a mut Uf,
}

impl Types<'_> {
    /// The type an already-rewritten node has, for the purpose of writing a signature.
    ///
    /// Deliberately shallower than inference: it reads types off things that *carry* one and answers
    /// [`Ty::Opaque`] for everything else, including arithmetic and `CASE`. Being wrong here
    /// declares a function over the wrong sort rather than asserting anything about the data, and
    /// `Opaque` is the fail-safe — it supports equality and nothing else, so a use that needed an
    /// order fails loudly instead of assuming one.
    fn of(&mut self, e: &Expr) -> Ty {
        match e {
            Expr::Nested(inner) => self.of(inner),
            Expr::Identifier(_) | Expr::CompoundIdentifier(_) => {
                match self.col.get(&nid(e)).cloned() {
                    Some(a) => self.uf.get_type(&a).unwrap_or(Ty::Opaque),
                    None => Ty::Opaque,
                }
            }
            Expr::Value(v) => crate::infer::literal_type(&v.value).unwrap_or(Ty::Opaque),
            Expr::Cast { data_type, .. } => {
                map_type_name(&data_type.to_string()).0.unwrap_or(Ty::Opaque)
            }
            Expr::Function(f) => {
                let name = obj_name(&f.name);
                builtin_rtype(&name)
                    .or_else(|| self.fn_ret.get(&nid(e)).copied())
                    .or_else(|| self.qcast_ret.get(&name.to_lowercase()).copied())
                    .or_else(|| {
                        let n: u32 = name.to_lowercase().strip_prefix("qp")?.parse().ok()?;
                        self.params.get(&n).copied()
                    })
                    .unwrap_or(Ty::Opaque)
            }
            _ => Ty::Opaque,
        }
    }
}

/// Build the `declare … function` table for the rewritten pair.
///
/// Three sources, matching the three kinds of symbol the preprocessor emits: one `qpN` per
/// parameter, one `qcastK` per shared conversion, and one entry per call whose result type nobody
/// declared. Keys are uppercased because that is how [`crate::catalog::parse_declare`] stores them
/// and how `lower.rs` looks them up — the two paths must agree or a declared return type silently
/// becomes the `INTEGER` default.
///
/// Must run *after* [`rewrite_casts`] and [`substitute_params`]: the `qcastK` return types are an
/// output of the rewrite, and a `qpN(0)` still spelled `$N` would contribute no call to declare.
pub fn declarations(
    queries: &[Query],
    inf: &mut Inferred,
    rw: &Rewrite,
) -> Result<HashMap<String, FnDecl>> {
    let mut out: HashMap<String, FnDecl> = HashMap::new();
    let decl = |t: Ty, aggregate: bool| FnDecl {
        ret: crate::types::normalize_type_name(t.sql()),
        aggregate,
    };
    for (n, t) in &inf.params {
        out.insert(format!("QP{n}"), decl(*t, false));
    }
    let mut qcast_ret: HashMap<String, Ty> = HashMap::new();
    for q in &rw.qcasts {
        out.insert(q.name.to_uppercase(), decl(q.ret, false));
        qcast_ret.insert(q.name.clone(), q.ret);
    }

    let Inferred { col, fn_ret, params, uf, .. } = inf;
    let mut ty = Types { col, fn_ret, params, qcast_ret, uf };

    // Every call the prover has no meaning for needs a signature, and the *same* symbol appearing
    // twice must get the same one -- two declarations of one name is not something the prover can
    // represent, so a genuine conflict is a refusal rather than last-writer-wins.
    let mut sigs: HashMap<String, Vec<Ty>> = HashMap::new();
    for q in queries {
        sweep(q, |e| {
            let Expr::Function(f) = e else { return Ok(()) };
            let Some(ret) = ty.fn_ret.get(&nid(e)).copied() else { return Ok(()) };
            let name = obj_name(&f.name);
            // Only positional value arguments, matching what `lower.rs` puts in the call's operand
            // list -- a declaration whose arity disagrees with the lowered call is exactly the kind
            // of mismatch the prover asserts on.
            let args: Vec<&Expr> = match &f.args {
                FunctionArguments::List(l) => l
                    .args
                    .iter()
                    .filter_map(|a| match a {
                        FunctionArg::Unnamed(FunctionArgExpr::Expr(x)) => Some(x),
                        _ => None,
                    })
                    .collect(),
                _ => Vec::new(),
            };
            // Nullary is refused rather than declared: the prover's DSL has no zero-argument form,
            // and giving it a filler argument would collapse two distinct calls into one constant.
            if args.is_empty() {
                return Err(unsupported(format!("nullary unknown function {name}")));
            }
            let argtypes: Vec<Ty> = args.iter().map(|a| ty.of(a)).collect();
            let key = name.to_uppercase();
            let d = decl(ret, is_agg_name(&name));
            if let Some(prev) = sigs.get(&key) {
                if *prev != argtypes || out.get(&key).is_some_and(|p| p.ret != d.ret) {
                    return Err(unsupported(format!(
                        "function {name} used with conflicting signatures"
                    )));
                }
            }
            sigs.insert(key.clone(), argtypes);
            out.insert(key, d);
            Ok(())
        })?;
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::infer::infer;
    use sqlparser::ast::Statement;
    use sqlparser::parser::Parser;

    /// Parse, infer, rewrite — and hand back the rendered queries plus what the rewrite produced.
    fn run(sql: &str) -> Result<(Vec<String>, Rewrite)> {
        let mut queries: Vec<Query> = Parser::parse_sql(&crate::DIALECT, sql)
            .expect("parses")
            .into_iter()
            .map(|s| match s {
                Statement::Query(q) => *q,
                other => panic!("not a query: {other}"),
            })
            .collect();
        let mut inf = infer(&queries, None)?;
        let rw = rewrite_casts(&mut queries, &mut inf)?;
        substitute_params(&mut queries, &mut inf)?;
        Ok((queries.iter().map(|q| q.to_string()).collect(), rw))
    }

    fn one(sql: &str) -> String {
        run(sql).expect("rewrites").0.remove(0)
    }

    #[test]
    fn a_cast_over_a_parameter_is_dropped() {
        // Rule 1. The parameter carries the type; the cast on it is the identity.
        let (q, rw) = run("SELECT * FROM t WHERE t.a = $1::integer").expect("rewrites");
        assert!(q[0].contains("qp1(0)"), "{}", q[0]);
        assert!(!q[0].to_uppercase().contains("CAST"), "{}", q[0]);
        assert_eq!(rw.dropped.param, 1);
    }

    #[test]
    fn a_cast_over_a_literal_keeps_the_cast_and_retargets_it() {
        // Rules 2/3: `decimal` is retargeted to `numeric`, the spelling `Ty::Real` reads back as.
        let q = one("SELECT t.a, CAST(1 AS decimal) FROM t");
        assert!(q.to_uppercase().contains("CAST(1 AS NUMERIC)"), "{q}");
    }

    #[test]
    fn an_identity_cast_is_dropped_but_a_narrowing_one_is_not() {
        // Rule 4 against rule 5b. Both sides of the distinction in one pair: `name` infers VARCHAR,
        // so `::varchar` is a no-op and `::varchar(8)` is a truncation.
        let (q, rw) = run(
            "SELECT t.name::varchar FROM t; SELECT t.name::varchar(8) FROM t",
        )
        .expect("rewrites");
        assert!(!q[0].to_uppercase().contains("CAST"), "{}", q[0]);
        assert_eq!(rw.dropped.identity, 1);
        assert!(q[1].contains("qcast0"), "{}", q[1]);
        assert_eq!(rw.qcasts, vec![QCast { name: "qcast0".into(), arg: Ty::Str, ret: Ty::Str }]);
    }

    #[test]
    fn two_different_truncations_are_not_one_symbol() {
        // The dedup key decides which conversions are *the same function*, so anything the target
        // says and the key drops becomes two operators sharing one symbol. A length qualifier says
        // where the string is cut: `varchar(8)` and `varchar(4)` are different functions of the
        // same column, and equating them would prove a rewrite that changes the cut.
        let (_, rw) =
            run("SELECT t.name::varchar(8) FROM t; SELECT t.name::varchar(4) FROM t")
                .expect("rewrites");
        assert_eq!(rw.qcasts.len(), 2, "two truncations collapsed to one symbol: {:?}", rw.qcasts);
    }

    #[test]
    fn widening_an_integer_to_a_real_stays_a_native_cast() {
        // Rule 5a: the prover understands this one, so it does not need a symbol. Only the target
        // is rewritten -- sqlparser keeps the `::` spelling, and `lower.rs` reads the target rather
        // than the spelling, so the two forms lower identically.
        let (q, rw) = run("SELECT t.user_id::numeric FROM t").expect("rewrites");
        assert!(q[0].to_uppercase().contains("T.USER_ID::NUMERIC"), "{}", q[0]);
        assert!(rw.qcasts.is_empty());
        // A float is not a REAL: it rounds. Its cast is a `qcast` into the opaque type.
        let (q, rw) = run("SELECT t.user_id::float FROM t").expect("rewrites");
        assert!(q[0].contains("qcast0"), "{}", q[0]);
        assert_eq!(rw.qcasts, vec![QCast { name: "qcast0".into(), arg: Ty::Int, ret: Ty::Opaque }]);
    }

    #[test]
    fn the_same_conversion_on_both_sides_shares_one_symbol() {
        // The point of the dedup key: a rewrite that leaves a conversion untouched must cancel, and
        // it only cancels if both sides name the same function.
        let (q, rw) = run(
            "SELECT t.name::integer FROM t; SELECT t.name::integer FROM t WHERE t.name = 'x'",
        )
        .expect("rewrites");
        assert!(q[0].contains("qcast0") && q[1].contains("qcast0"), "{q:?}");
        assert_eq!(rw.qcasts.len(), 1);
        assert_eq!(rw.dropped.qcast, 2);
    }

    #[test]
    fn different_targets_over_one_operand_get_different_symbols() {
        let (_, rw) = run("SELECT t.name::integer, t.name::boolean FROM t").expect("rewrites");
        assert_eq!(rw.qcasts.len(), 2);
        assert_eq!(rw.qcasts[0].ret, Ty::Int);
        assert_eq!(rw.qcasts[1].ret, Ty::Bool);
    }

    #[test]
    fn parentheses_do_not_hide_the_operand() {
        // `($1)::int` is still rule 1, not "cast over unsupported operand Expr".
        let (q, rw) = run("SELECT * FROM t WHERE t.a = ($1)::integer").expect("rewrites");
        assert_eq!(rw.dropped.param, 1);
        assert!(q[0].contains("qp1(0)"), "{}", q[0]);
    }

    #[test]
    fn an_untypeable_operand_takes_the_symbol_rather_than_the_refusal() {
        // Rules 4 and 5a are the only ones that read the operand's type, and both are unreachable
        // with `Ty::Opaque` -- `map_type_name` never yields `Some(Ty::Opaque)`, so the identity gate
        // cannot fire, and `Opaque != Int` kills the widening. So an operand nobody can type lands
        // on 5b, which names the conversion instead of dropping or retargeting it.
        let (q, rw) = run("SELECT (t.a + 1)::integer FROM t").expect("rewrites");
        assert_eq!(q[0], "SELECT qcast0(t.a + 1) FROM t");
        assert_eq!(rw.dropped.identity, 0);
        assert_eq!(rw.qcasts, vec![QCast { name: "qcast0".into(), arg: Ty::Opaque, ret: Ty::Int }]);
    }

    #[test]
    fn a_cast_states_its_own_type_for_the_cast_above_it() {
        // A nested cast is the *most* precise operand there is: the SQL says what it produces. Here
        // that makes the outer a real conversion (VARCHAR from INTEGER), so both casts survive as
        // symbols -- the inner because `name` infers VARCHAR, the outer because Str != Int.
        // `decide` sweeps top-down, so the *outer* cast is numbered first.
        let (q, rw) = run("SELECT t.name::integer::varchar FROM t").expect("rewrites");
        assert_eq!(q[0], "SELECT qcast0(qcast1(t.name)) FROM t");
        assert_eq!(rw.qcasts.len(), 2, "{:?}", rw.qcasts);
        assert_eq!(rw.qcasts[0].ret, Ty::Str, "outer target");
        assert_eq!(rw.qcasts[1].ret, Ty::Int, "inner target");
        // The point of the arm: the outer read `Int` off the inner's target, not off `name`.
        assert_eq!(rw.qcasts[0].arg, Ty::Int);
    }

    #[test]
    fn chained_identity_casts_walk_the_column_binding_to_its_final_home() {
        // Both casts are no-ops over an INTEGER column, so both hoist. `Apply` pushes
        // `(operand, inner slot)` before `(inner slot, outer slot)` and `rewrite_casts` replays them
        // in that order, so the `col` entry follows the chain. Push order *is* the argument -- swap
        // the two and the binding is dropped instead of moved.
        let mut queries: Vec<Query> = Parser::parse_sql(
            &crate::DIALECT,
            "SELECT t.user_id::integer::integer FROM t",
        )
        .expect("parses")
        .into_iter()
        .map(|s| match s {
            Statement::Query(q) => *q,
            other => panic!("not a query: {other}"),
        })
        .collect();
        let mut inf = infer(&queries, None).expect("infers");
        let rw = rewrite_casts(&mut queries, &mut inf).expect("rewrites");
        assert_eq!(queries[0].to_string(), "SELECT t.user_id FROM t");
        assert_eq!(rw.dropped.identity, 2);
        // The surviving node is the one the outer cast used to occupy, and inference still knows
        // which column it is.
        let mut found = 0;
        let _ = sqlparser::ast::visit_expressions(&queries[0], |e| {
            if matches!(e, Expr::CompoundIdentifier(_)) && inf.col.contains_key(&nid(e)) {
                found += 1;
            }
            ControlFlow::<()>::Continue(())
        });
        assert_eq!(found, 1, "the column binding did not survive the chained hoist");
    }

    #[test]
    fn a_widening_cast_over_a_truncating_one_is_still_an_identity() {
        // `varchar(8)` produces a VARCHAR, so casting that to unqualified `varchar` is a no-op and
        // rule 4 takes the *outer*. The truncation underneath must not be touched.
        let (q, rw) = run("SELECT t.name::varchar(8)::varchar FROM t").expect("rewrites");
        assert_eq!(rw.dropped.identity, 1);
        assert_eq!(q[0], "SELECT qcast0(t.name) FROM t", "the truncation was dropped");
        assert_eq!(rw.qcasts.len(), 1);
    }

    #[test]
    fn a_qualified_target_over_a_cast_is_not_an_identity() {
        // The mirror: `varchar` then `varchar(8)` cuts the string, so the outer has to survive even
        // though both sides read VARCHAR. Rule 4's `!qualified` condition is what saves it.
        let (q, rw) = run("SELECT t.name::varchar::varchar(8) FROM t").expect("rewrites");
        assert_eq!(rw.dropped.identity, 1, "the inner no-op should still go");
        assert!(q[0].contains("qcast0"), "{}", q[0]);
        assert_eq!(rw.qcasts[0].ret, Ty::Str);
    }

    #[test]
    fn a_cast_over_a_modelled_call_no_longer_refuses() {
        // The operand arm used to be gated on `is_opaque_call`, which excluded exactly the functions
        // whose return type is known. An aggregate is the common case in this corpus.
        let (q, rw) = run("SELECT sum(t.user_id)::numeric FROM t").expect("rewrites");
        assert!(q[0].contains("qcast0(sum(t.user_id))"), "{}", q[0]);
        assert_eq!(rw.qcasts.len(), 1);
    }

    #[test]
    fn an_array_or_tuple_operand_still_refuses() {
        // Not conservatism for its own sake: under `= ANY` a `qcast` around an array literal would
        // cost `lower_quantified`'s exact OR-expansion of it, and `lower_expr` has no arm for a
        // tuple, so a `qcast` around one would move the refusal rather than lift it.
        let e = run("SELECT t.a FROM t WHERE t.a = ANY(ARRAY[$1]::bigint[])")
            .unwrap_err()
            .to_string();
        assert!(e.contains("cast over unsupported operand Array"), "{e}");
        let e = run("SELECT (t.a, t.b)::record FROM t").unwrap_err().to_string();
        assert!(e.contains("cast over unsupported operand Tuple"), "{e}");
    }

    #[test]
    fn an_unmappable_target_becomes_an_opaque_carrier() {
        // The conversion is still a function of its operand; only its meaning is out of reach. So it
        // gets a symbol with an equality-only sort on both ends rather than refusing the pair, and two
        // sides that agree on the cast cancel it.
        let (q, rw) = run("SELECT t.a::jsonb FROM t").expect("rewrites");
        assert_eq!(q[0], "SELECT qcast0(t.a) FROM t");
        assert_eq!(rw.qcasts.len(), 1);
        assert_eq!(data_type(rw.qcasts[0].ret).to_string(), "VARBINARY");
        assert_eq!(rw.qcasts[0].ret, Ty::Opaque);
    }

    #[test]
    fn two_different_unmappable_targets_are_not_one_symbol() {
        // Every unmappable target carries as `Opaque`, so `arg` and `ret` alone cannot tell `jsonb`
        // from `uuid[]` -- and a Z3 uninterpreted function *is* its name plus its sorts. If these
        // shared a symbol the pair would prove that converting a column to JSON and to a UUID array
        // give the same value.
        let (q, rw) = run("SELECT t.a::jsonb FROM t; SELECT t.a::uuid[] FROM t").expect("rewrites");
        assert_eq!(rw.qcasts.len(), 2, "two targets collapsed to one symbol: {:?}", rw.qcasts);
        assert_ne!(q[0], q[1]);
    }

    #[test]
    fn an_unmappable_cast_is_never_an_identity() {
        // The identity rule drops a cast whose target the operand already has. Both sides here read
        // `Opaque` -- an unknown column, an unreadable target -- and letting them match would drop the
        // cast outright, proving `a::jsonb` equal to a bare `a`.
        let (q, rw) = run("SELECT a::jsonb FROM t").expect("rewrites");
        assert_eq!(rw.dropped.identity, 0, "an unreadable cast was dropped as an identity");
        assert_eq!(q[0], "SELECT qcast0(a) FROM t");
    }

    #[test]
    fn two_spellings_of_one_target_still_share_a_symbol() {
        // The point of deduping in the first place: a pair whose two sides merely respell a cast has
        // to cancel it, so a mappable target collapses to its type and `text` meets `varchar`.
        let (q, rw) =
            run("SELECT t.n::text FROM t; SELECT t.n::varchar FROM t").expect("rewrites");
        assert_eq!(rw.qcasts.len(), 1, "{:?}", rw.qcasts);
        assert_eq!(q[0], q[1]);
    }

    /// Parse, infer, rewrite, substitute and synthesize — the whole tail of the preprocessor.
    fn decls(sql: &str) -> Result<HashMap<String, FnDecl>> {
        let mut queries: Vec<Query> = Parser::parse_sql(&crate::DIALECT, sql)
            .expect("parses")
            .into_iter()
            .map(|s| match s {
                Statement::Query(q) => *q,
                other => panic!("not a query: {other}"),
            })
            .collect();
        let mut inf = infer(&queries, None)?;
        let rw = rewrite_casts(&mut queries, &mut inf)?;
        substitute_params(&mut queries, &mut inf)?;
        declarations(&queries, &mut inf, &rw)
    }

    #[test]
    fn every_synthesized_symbol_is_declared() {
        // One of each: a parameter, a conversion, and a call nobody modelled.
        let d = decls("SELECT q_op_f(t.name::integer) FROM t WHERE t.user_id = $1").expect("ok");
        assert_eq!(d["QP1"].ret, "INTEGER");
        assert_eq!(d["QCAST0"].ret, "INTEGER");
        assert_eq!(d["Q_OP_F"].ret, "VARBINARY");
        assert!(!d["Q_OP_F"].aggregate);
    }

    #[test]
    fn an_aggregate_symbol_is_declared_as_an_aggregate() {
        // The `qa_` prefix is the only thing separating an aggregate from a scalar here, and
        // getting it wrong turns one output row into one row per input row.
        let d = decls("SELECT qa_int_total(t.user_id) FROM t").expect("ok");
        assert_eq!(d["QA_INT_TOTAL"].ret, "INTEGER");
        assert!(d["QA_INT_TOTAL"].aggregate);
    }

    #[test]
    fn the_declared_return_type_matches_what_parse_declare_would_read() {
        // `Ty::Real` spells itself DOUBLE, but the DSL parser normalises DOUBLE to REAL. If these
        // two paths disagree the same pair gets different IR depending on which produced it.
        let d = decls("SELECT q_op_f(t.amount) FROM t WHERE t.amount = 1.5").expect("ok");
        let (_, parsed) =
            crate::catalog::parse_declare("declare scalar function f(double) returns double;")
                .expect("parses");
        assert_eq!(d["Q_OP_F"].ret, "VARBINARY");
        assert_eq!(parsed.ret, "REAL");
        assert_eq!(crate::types::normalize_type_name(Ty::Real.sql()), parsed.ret);
    }

    #[test]
    fn one_symbol_used_two_ways_refuses_the_pair() {
        let e = decls("SELECT q_op_f(t.user_id), q_op_f(t.name) FROM t").unwrap_err().to_string();
        assert!(e.contains("conflicting signatures"), "{e}");
    }

    #[test]
    fn a_nullary_unknown_function_refuses_the_pair() {
        let e = decls("SELECT t.a, q_int_seq() FROM t").unwrap_err().to_string();
        assert!(e.contains("nullary unknown function"), "{e}");
    }

    #[test]
    fn a_rewritten_operand_keeps_its_attribution() {
        // The re-keying in `rewrite_casts`. `t.name::varchar` is an identity cast, so `t.name` moves
        // into the cast's slot; if its entry were not moved with it, the signature synthesis that
        // runs next would type the column Opaque instead of VARCHAR.
        let sql = "SELECT q_op_f(t.name::varchar) FROM t";
        let mut queries: Vec<Query> = Parser::parse_sql(&crate::DIALECT, sql)
            .expect("parses")
            .into_iter()
            .map(|s| match s {
                Statement::Query(q) => *q,
                other => panic!("not a query: {other}"),
            })
            .collect();
        let mut inf = infer(&queries, None).expect("infers");
        rewrite_casts(&mut queries, &mut inf).expect("rewrites");
        let found = inf
            .col
            .values()
            .any(|a| matches!(a, Atom::Col(t, c) if t == "t" && c == "name"));
        assert!(found, "the hoisted column lost its attribution: {:?}", inf.col);
    }
}
