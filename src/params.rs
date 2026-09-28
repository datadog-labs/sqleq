// Unless explicitly stated otherwise all files in this repository are licensed under the
// Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/).
// Copyright 2026-Present Datadog, Inc.

//! Does `$N` on one side mean the same value as `$N` on the other?
//!
//! Every parameterized pair carries an assumption the input never states. The frontend lowers `$N`
//! to a single shared symbol `qpN` ([`crate::casts::substitute_params`]), so the pair it hands the
//! prover asks: *for every value of `qp1, qp2, …`, do the two queries agree?* That is **index
//! binding** — `$1` on the left is `$1` on the right because they share a number.
//!
//! The question the caller means is **intended binding**: `$1` on the left is whichever placeholder
//! on the right the application fills from the same value. The two coincide only when the pair's two
//! sides were numbered from the same call site, and a rewrite that drops, adds, or reorders a
//! placeholder renumbers everything after it. When they diverge, index binding is a *different
//! question* — and the prover can answer it correctly while the answer says nothing about the pair
//! the caller has.
//!
//! Nothing in the input distinguishes the two, because the frontend never sees the call site. So it
//! assumes index binding, and this module looks for evidence that the assumption is wrong. Two
//! sub-reasons, of very different strength:
//!
//! * **`arity`** — the two sides mention different sets of `$N`, *and* share at least one. Exact, no
//!   heuristics. (The label is a word wider than the arithmetic: `{$1,$2}` against `{$1,$3}` has
//!   equal *count* and still reports `arity`, because what fails is the correspondence, not the
//!   tally.)
//! * **`order`** — same set, but some `$k` is compared against a disjoint set of base columns on the
//!   two sides. Evidence of a permutation. **Best effort**: see the limits below.
//!
//! ## Why differing sets are only suspicious when they overlap
//!
//! Sets that differ *disjointly* are safe, and this is the one place the reasoning has to be spelled
//! out because the obvious rule — "different sets, refuse" — refuses sound pairs. When no index
//! occurs on both sides, index binding quantifies the two queries over **independent** values, which
//! is a *stronger* statement than any intended binding that identifies some of them. `WHERE a = $1`
//! against `WHERE a = $2` is asked as `∀ qp1, qp2` and comes back unprovable, where the caller's
//! `∀ v` is a true equivalence — incomplete, never unsound. `LIMIT $1 OFFSET $2` against a side with
//! no parameters at all is the same shape, and it is the one the crate's own
//! `a_parameter_is_a_count` test pins: the broad rule refuses it, and that refusal would be wrong.
//! The price of the broad rule is small — a handful of extra refusals, every one on a pair nothing
//! else could decide either — so the case for the overlap condition is the argument above, not the
//! measurement. Most pagination rewrites keep a filter parameter too, which makes the sets *overlap*
//! and fires either rule.
//!
//! Overlap is what turns a difference into a hazard. A shared `$k` is lowered to *one* symbol, so if
//! the caller in fact fills query A's `$k` and query B's `$k` from different values, the prover checks
//! only the diagonal of the space it should have covered — a special case, reported as a general
//! proof. One observed pair has exactly that shape: seven placeholders against two, sharing `$1` and
//! `$2`, with `$1` a `count()` argument on one side and a plain column reference on the other.
//! Different-and-overlapping is therefore the exact predicate, not a conservative approximation.
//!
//! ## Why this is a refusal and not a claim of non-equivalence
//!
//! Tempting, since a renumbered pair usually *is* non-equivalent under index binding — the pair just
//! described is, and a two-row table witnesses it. But not always: another observed pair's extra `$3`
//! occurs only as `SELECT $3` inside an `EXISTS`, where nothing observes it, and it is equivalent
//! under either binding. Misalignment is evidence about the *question*, not about the answer, so this
//! reports its own refusal reason rather than a verdict either way. The caller's fix is to renumber
//! the pair or to say what the mapping is — not to trust a verdict we would have had to invent.
//!
//! ## What `order` cannot catch
//!
//! A permutation is invisible here whenever the evidence for the two roles looks the same. If both
//! `$1` and `$2` are compared against the same column, or against two columns of the same inferred
//! type in positions this walk cannot tell apart, the role sets overlap (or are empty) and nothing
//! fires. That is the residual soundness hole, and it is documented as such in the README's
//! Soundness section rather than papered over: a missed misalignment can let the prover report
//! `provable` for a pair that is not equivalent under the binding the caller intended.
//!
//! The opposite error is a cost, not a hole. A legitimate rewrite that moves a comparison onto a
//! join partner — `a.id = $1 AND a.id = b.a_id` against `b.a_id = $1` — has genuinely disjoint role
//! sets and reports `order` even though the pair is fine. That loses a proof, which is the trade
//! this crate makes everywhere.
//!
//! ## What it costs
//!
//! Both halves were measured over a corpus before they shipped. The figures are not reproducible from
//! this repository, but the shape of what they showed is the argument for the design.
//!
//! `arity` fires on a small fraction of rows and `order` on none of them. A few of the rows it takes
//! away were previously proofs: one is the confirmed false proof this reason exists to stop, and the
//! rest are benign losses — pairs equivalent under either binding, now refused because nothing in the
//! pair says which binding was meant.
//!
//! Most of the rows it refuses are rows nothing else would have refused, which is what raising the
//! verdict after lowering buys. The remainder are the precedence rule: a renumbering produced a
//! `type conflict` ([`root_cause`](crate::params::root_cause)) or mistyped a `LIMIT` count ([`root_cause_lowered`](crate::params::root_cause_lowered)), and the
//! misalignment is now reported instead of the symptom it caused. No row changes *status* in either
//! direction — the same refused set, differently labelled — and every emitted case file is
//! byte-identical across the change.
//!
//! One pair was being refused for a misalignment `strip_identical_pagination` had itself created,
//! which is why [`mentioned`](crate::params::mentioned) takes its snapshot above the normalizations rather than below. Both its
//! sides lower and neither is a proof — the prover returns `provable: false` outside its complete
//! fragment and sqleq-fuzz returns `NO-COUNTEREXAMPLE` — so it lands in the cell where the two axes
//! have nothing to say about each other, and capability does not move.
//!
//! The rule is not a rubber stamp, which was measured rather than argued: of the rows that reach
//! lowering with a verdict pending, nearly all *yield*, every one to a construct or an unresolved name
//! — window functions, row comparisons, set-returning functions, columns that resolve nowhere — none
//! of which a renumbering could have produced.
//!
//! `order`'s zero is not the detector being inert, which was checked rather than assumed: plenty of
//! pairs reach it with at least one shared parameter carrying role evidence on *both* sides, and most
//! shared parameter slots are doubly evidenced. What was measured has no *detectable* permutation. It
//! says nothing about the undetectable ones.
//!
//! One refinement is tempting and a real pair refutes it: "the orphan indices all sit above every
//! shared index, so dropping them cannot have renumbered the shared ones." That would keep the benign
//! losses — and it would also readmit the false proof, whose orphans `$3..$7` are all above the shared
//! `$1` and `$2` while `$1` is a `count()` argument on one side and a column reference on the other.
//! The rule is exactly wrong on the one case that matters.
//!
//! ## When each half runs
//!
//! The two sub-reasons need different things, so they are two functions and they run at different
//! points.
//!
//! [`check_arity`](crate::params::check_arity) needs only the `$N` each query mentions, which is in the text. It runs **before
//! inference**, because inference is one of the things a misalignment can make fail: a pair whose
//! placeholders were renumbered gets two unrelated values in one type class, and what surfaces is a
//! `type conflict` at a parameter that is not itself wrong. [`root_cause`](crate::params::root_cause) is where that ordering is
//! decided, and it decides it by asking whether the failure survives the two queries' parameters being
//! pulled apart.
//!
//! [`check_roles`](crate::params::check_roles) reads inference's own attribution, so it runs between
//! [`crate::casts::rewrite_casts`] and [`crate::casts::substitute_params`]: the first has already
//! hoisted `$N::T` to a bare `$N` (rule 1), so a cast cannot hide a parameter from the operand test,
//! and the second deletes the [`Inferred::col`](crate::infer::Inferred::col) attribution the role walk reads.
//!
//! Either verdict is then *held* and raised only after both queries have lowered, so a construct the
//! frontend cannot lower — which no misalignment produced — is still what the row reports.
//!
//! ## The two gates, and the one rule they share
//!
//! Holding the verdict makes lowering a second gate, and a refusal there gets the same treatment
//! inference's did: **a misalignment outranks any refusal it could have manufactured, and yields to any
//! refusal it could not.** [`root_cause`](crate::params::root_cause) applies it to inference failing, [`root_cause_lowered`](crate::params::root_cause_lowered) to
//! lowering failing, and both decide by the same counterfactual — pull the two queries' parameters
//! apart ([`crate::infer::split_params`]) and see whether the refusal survives. A refusal that *goes
//! away* was the frontend's own index binding talking, and reporting it sends the caller after a type,
//! or a count, that is not what is wrong.

use std::collections::{BTreeMap, BTreeSet};
use std::ops::ControlFlow;

use sqlparser::ast::{
    Array, ArrayElemTypeDef, BinaryOperator, DataType, Expr, FunctionArg, FunctionArgExpr,
    FunctionArguments, ObjectName, Query, SetExpr, Statement, TableFactor, Visit, Visitor,
};

use crate::casts::unwrap_nested;
use crate::catalog::{obj_name, Catalog};
use crate::error::{misaligned, FrontendError, Result};
use crate::infer::{infers_with_split_params, nid, param_index, sweep, Atom, Inferred, Origin};
use crate::normalize::array_elem_type;

/// A base column, `(table, column)` — the unit of evidence for what role a parameter plays.
type Col = (String, String);

/// What one query does with its parameters.
#[derive(Default)]
struct Side {
    /// Every `$N` the query mentions, in any position.
    seen: BTreeSet<u32>,
    /// `$N` → the base columns it is compared against. Absent or empty means no evidence, which is
    /// not the same as "no role" and never fires anything.
    roles: BTreeMap<u32, BTreeSet<Col>>,
}

/// The comparisons whose two operands are about the same thing, so one names the other's role.
///
/// The six [`crate::infer`]'s comparison sweep unifies on, for the same reason it does: these are the
/// operators under which a parameter and a column have to be talking about one value.
fn is_cmp(op: &BinaryOperator) -> bool {
    matches!(
        op,
        BinaryOperator::Eq
            | BinaryOperator::NotEq
            | BinaryOperator::Gt
            | BinaryOperator::Lt
            | BinaryOperator::GtEq
            | BinaryOperator::LtEq
    )
}

/// The parameters an operand *is*, at the granularity a comparison reaches.
///
/// The operand itself, or the elements of an array literal it spells out — so `x = ANY($1)` and
/// `x = ANY(ARRAY[$1, $2])` both attribute `x` to every parameter compared against it. Deliberately
/// not a subtree walk: `$1 + $2 = x` does not make either parameter play `x`'s role, and recording
/// that it does would blur exactly the distinction this module exists to draw.
fn operand_params(e: &Expr) -> Vec<u32> {
    let e = unwrap_nested(e);
    if let Some(n) = param_index(e) {
        return vec![n];
    }
    match e {
        Expr::Array(Array { elem, .. }) => {
            elem.iter().filter_map(|x| param_index(unwrap_nested(x))).collect()
        }
        _ => Vec::new(),
    }
}

/// The base column an operand names, if it names one.
///
/// Read off [`Inferred::col`] rather than off the text: attribution has already resolved aliases and
/// bare names to `(base table, column)`, and re-deriving that from the SQL is what three rounds of
/// regex over the corpus got wrong. `None` for anything that is not a resolvable column — a
/// literal, a call, an expression — which costs evidence and never invents any.
///
/// A **cast** is one of those shapes, and it is the one that actually occurs: `col::T = $N` gives `$N`
/// no role, so a swap between two cast columns is missed. The error runs in the unsound direction, so
/// the bound is measured rather than argued — a corpus scan found every slot blinded this way, looked
/// through the cast at each one, and found none of them misaligned.
/// Note the loss is *not* an artifact of running after [`crate::casts::rewrite_casts`]: the pre-rewrite tree
/// is an `Expr::Cast`, equally unmatched here, and the rewrite's rule 4 no-op *restores* the evidence
/// by deleting the cast. `tests/lower.rs` pins both halves.
fn role_of(e: &Expr, inf: &Inferred) -> Option<Col> {
    let e = unwrap_nested(e);
    match e {
        Expr::Identifier(_) | Expr::CompoundIdentifier(_) => match inf.col.get(&nid(e)) {
            Some(Atom::Col(t, c)) => Some((t.clone(), c.clone())),
            _ => None,
        },
        _ => None,
    }
}

/// Record `other`'s column, if it has one, as the role of every parameter in `operand`.
///
/// `inf` is `None` when the caller only wants the parameter *set* ([`check_arity`]), which is the case
/// on a pair inference could not type at all.
fn attribute(operand: &Expr, other: &Expr, inf: Option<&Inferred>, side: &mut Side) {
    let Some(inf) = inf else { return };
    let ps = operand_params(operand);
    if ps.is_empty() {
        return;
    }
    let Some(col) = role_of(other, inf) else { return };
    for n in ps {
        side.roles.entry(n).or_default().insert(col.clone());
    }
}

/// Walk one query: which parameters it mentions, and — given inference's attribution — what each is
/// compared against.
fn side_of(q: &Query, inf: Option<&Inferred>) -> Side {
    let mut side = Side::default();
    // The closure never refuses, so the sweep cannot fail; `sweep`'s signature carries the `Result`
    // for the rules that do.
    let _ = sweep(q, |e| {
        if let Some(n) = param_index(e) {
            side.seen.insert(n);
        }
        match e {
            Expr::BinaryOp { left, op, right } if is_cmp(op) => {
                attribute(left, right, inf, &mut side);
                attribute(right, left, inf, &mut side);
            }
            Expr::InList { expr, list, .. } => {
                for item in list {
                    attribute(item, expr, inf, &mut side);
                }
            }
            Expr::AnyOp { left, right, .. } | Expr::AllOp { left, right, .. } => {
                attribute(right, left, inf, &mut side);
            }
            Expr::Between { expr, low, high, .. } => {
                attribute(low, expr, inf, &mut side);
                attribute(high, expr, inf, &mut side);
            }
            Expr::Like { expr, pattern, .. } | Expr::ILike { expr, pattern, .. } => {
                attribute(pattern, expr, inf, &mut side);
            }
            _ => {}
        }
        Ok(())
    });
    side
}

/// `$1, $2, $3` — bounded, because a query can mention more parameters than a message should carry.
fn list(ns: impl IntoIterator<Item = u32>) -> String {
    let ns: Vec<u32> = ns.into_iter().collect();
    let shown: Vec<String> = ns.iter().take(8).map(|n| format!("${n}")).collect();
    let mut s = shown.join(", ");
    if ns.len() > 8 {
        s.push_str(&format!(", … ({} more)", ns.len() - 8));
    }
    s
}

fn cols(cs: &BTreeSet<Col>) -> String {
    cs.iter().map(|(t, c)| format!("{t}.{c}")).collect::<Vec<_>>().join(", ")
}

/// The `$N` each query mentions, for [`check_arity`] to compare.
///
/// Called on the pair **as parsed**, before the normalizations, and that is not an optimisation — it is
/// the fix for a false positive the ordering caused. `normalize::strip_identical_pagination` deletes a
/// `LIMIT` that is textually identical on both sides, which deletes the parameter in it from whichever
/// side mentioned it *only* there. An observed pair has that shape: `… LIMIT $4` against
/// `(… LIMIT $4) UNION ALL (… LIMIT $4) LIMIT $4`. The strip is pair-level and reaches only the two
/// outermost queries, so query A loses its only `$4` while query B keeps the two inside the set
/// operation — `{1,2,3}` against `{1,2,3,4}`, overlapping, and `arity` fired on a pair whose two texts
/// both mention `$1..$4`. The misalignment was manufactured by this crate's own rewrite.
///
/// So the snapshot is taken where the pair is *decided* and before any equivalence-preserving rewrite
/// that can delete a placeholder — `strip_identical_pagination`, `strip_dead_order_by`, and an
/// `inline_ctes` that drops an unused binding. Two rewrites above the cut are safe on inspection but not
/// relied on: `dml::reduce` decides what the pair even is rather than tidying it, and `demote_operators`
/// and `fix_precedence` move operands without removing any.
///
/// This is the same principle [`root_cause`] applies one level up — *a refusal the frontend's own
/// assumption manufactured is not the pair's fault* — arriving here by construction rather than by a
/// counterfactual, because a set the check never sees cannot be reasoned about wrongly.
pub fn mentioned(queries: &[Query]) -> Vec<BTreeSet<u32>> {
    queries.iter().map(|q| side_of(q, None).seen).collect()
}

/// The `arity` sub-reason: do the two queries mention *corresponding* sets of `$N`?
///
/// Syntactic — the `$N` a query mentions is in its text — so this has a verdict on any pair that
/// parses, including one inference cannot type. That is not incidental: it is what lets
/// [`root_cause`] ask which of the two refusals came first in the causal order. Split out from
/// [`check_roles`] for exactly that reason, and kept free of [`Inferred`] so it cannot drift back into
/// depending on it.
///
/// Takes the sets from [`mentioned`] rather than the queries, so that what it compares is the pair the
/// caller wrote and not the pair the normalizations left behind. Passing trees instead is what produced
/// the pagination false positive above, and taking a value the checker cannot recompute is what stops
/// that from being reintroduced by a later stage moving.
///
/// The only error it returns is [`crate::FrontendError::ParameterMisaligned`], which is why
/// [`crate::lower_sql`]'s pipeline can hold it in an `Option` and raise it after lowering. It is
/// raised late so the bucket means something: a row lands in `parameter-misaligned` only when nothing
/// *else* would have refused it — with the one exception [`root_cause`] documents, where the something
/// else is a refusal this misalignment produced. The cost is that a pair with both problems reports the
/// construct first, and the misalignment surfaces only once that is fixed — the same
/// first-refusal-only behaviour the rest of the frontend has.
pub fn check_arity(seen: &[BTreeSet<u32>]) -> Result<()> {
    // `parse_input` guarantees two, but this module must not be the one that panics if that changes.
    let [a, b] = seen else {
        return Ok(());
    };
    let shared: BTreeSet<u32> = a.intersection(b).copied().collect();

    // Different sets, sharing an index: no total correspondence exists, and the indices they do share
    // are the ones a renumbering would have moved. See the module docs for why the overlap condition
    // is not a weakening.
    if a != b && !shared.is_empty() {
        let orphans: Vec<u32> = a.symmetric_difference(b).copied().collect();
        return Err(misaligned(format!(
            "arity: {} uses {} parameter(s), {} uses {}, sharing {}; in one query {}",
            Origin::query(0).label(),
            a.len(),
            Origin::query(1).label(),
            b.len(),
            list(shared.iter().copied()),
            list(orphans)
        )));
    }
    Ok(())
}

/// The `order` sub-reason: the same set of `$N` on both sides, but a `$k` playing two different roles.
///
/// Reads [`Inferred::col`], so unlike [`check_arity`] it exists only where inference *succeeded* — a
/// pair that failed to type has no attribution to compare, which is why [`root_cause`] has to consider
/// `arity` alone.
pub fn check_roles(queries: &[Query], inf: &Inferred) -> Result<()> {
    if queries.len() != 2 {
        return Ok(());
    }
    let (a, b) = (side_of(&queries[0], Some(inf)), side_of(&queries[1], Some(inf)));
    let shared: BTreeSet<u32> = a.seen.intersection(&b.seen).copied().collect();

    for n in &shared {
        let (Some(ra), Some(rb)) = (a.roles.get(n), b.roles.get(n)) else { continue };
        if ra.is_empty() || rb.is_empty() || !ra.is_disjoint(rb) {
            continue;
        }
        return Err(misaligned(format!(
            "order: ${n} is compared against {} in {} but {} in {}",
            cols(ra),
            Origin::query(0).label(),
            cols(rb),
            Origin::query(1).label()
        )));
    }
    Ok(())
}

/// The `shape` sub-reason: does some `$k` want a *row value* on one side and an *array* on the other?
///
/// The shape this exists for is a single rewrite family, and once the malformed half of it is set
/// aside it is still large enough to dominate the top refusal families of a real capture:
///
/// ```sql
/// A: INSERT INTO findings (id, finding_id, original_status, main_resource_id, scan_id, editable)
///    VALUES ($1, $2, $3, $4, $5, $6), ($1, $2, $3, $4, $5, $6)
/// B: …same column list… SELECT * FROM unnest($1::text[], $2::text[], …, $6::text[])
/// ```
///
/// Both sides mention exactly `$1..$6`, so [`check_arity`] passes; an `INSERT`'s `VALUES` parameters
/// are compared against nothing, so [`check_roles`] passes vacuously. Yet `$1` is a `text` on A and a
/// `text[]` on B. The pair is not one question that the frontend cannot answer — it is two questions.
///
/// ## Why this one is raised *early*, when the other two are held
///
/// [`check_arity`] and [`check_roles`] are raised last, so that `parameter-misaligned` counts only
/// rows nothing else refused, and [`root_cause`]/[`root_cause_lowered`] then hand a construct refusal
/// precedence over a misalignment it could not have manufactured. This sub-reason is raised in
/// [`crate::parse_input`] instead, ahead of even [`crate::dml::reduce`] — the opposite end of the
/// pipeline. Three facts make that the right place and none of them holds for the other two:
///
/// * **It is terminal.** The refusals it pre-empts are capability statements: `INSERT … ON CONFLICT`,
///   `INSERT omits <col>`, `no base tables`. Implementing all three would still not decide these rows,
///   because no binding makes `$1` simultaneously a `text` and a `text[]`. Reporting a construct here
///   points the roadmap at work that cannot pay — which is the same objection [`root_cause`] raises
///   against reporting a manufactured `type conflict`, arriving from the other direction.
/// * **The refusals it pre-empts are raised before [`check_arity`] can run at all.** Two of the three
///   are `dml::reduce` refusals, and `reduce` runs inside [`crate::parse_input`] *above*
///   [`mentioned`]. So for most of these rows there is no held verdict to order against: the
///   pipeline never reaches the point where a misalignment could be noticed. Ordering was never the
///   mechanism keeping them in their bucket; reachability was.
/// * **It is syntactic.** Like [`check_arity`] and unlike [`check_roles`], it reads the text and needs
///   neither a catalog nor inference, so it has an answer at the top of the pipeline.
///
/// The counterfactual the other two are decided by does *not* license this, and it is worth being
/// explicit that the rule is not being read as endorsing it: pulling the two queries' parameters apart
/// ([`crate::infer::split_params`]) does remove a shape conflict, so a literal application of *"a
/// misalignment outranks any refusal it could have manufactured, and yields to any refusal it could
/// not"* would have this yield to `ON CONFLICT`. The claim here is narrower and different in kind — a
/// pair whose two halves take different argument types is not a well-posed equivalence question, and
/// well-posedness is prior to capability the same way malformed SQL is: a `VALUES` list whose tuples
/// do not all have the same arity is not a hard pair, it is not a pair. That is a *new* stage, not an
/// inversion of the existing rule, which is why the rule's text
/// and both its call sites are untouched.
///
/// ## What counts as which
///
/// Deliberately narrow, because the direction this can be wrong in is refusing a pair that is really
/// one question:
///
/// * **Row value** — the parameter is an element of an `INSERT`'s `VALUES` row. Casts to a non-array
///   type are read through (`VALUES ($1::text)` is still a row value); a cast to an array type makes
///   it an array instead, which is the honest reading of `VALUES ($1::text[])`.
/// * **Array** — the parameter is a positional argument of `unnest(…)`, or is cast to an array type.
///   Not "any array-typed position": `= ANY($1)` and `$1 @> …` are array-typed too, and they occur on
///   *both* sides of pairs that are perfectly well posed, so including them would buy nothing and risk
///   the only thing worth protecting.
/// * **Neither, or both** — nothing fires. A parameter that this walk finds in both roles within one
///   query is that query's own business, and a disagreement needs two unambiguous readings.
///
/// Runs before [`crate::dml::reduce`] and before the normalizations for the same reason
/// [`mentioned`] is read where it is: the `VALUES` list and the `unnest` call are the caller's own
/// text there, and no rewrite has yet had a chance to move either.
pub fn check_shape(statements: &[Statement]) -> Result<()> {
    let sides: Vec<BTreeMap<u32, Shape>> =
        statements.iter().map(shapes_of).filter(|m| !m.is_empty()).collect();
    // Not two parameterized statements: `parse_input` decides what the pair is, and this check does
    // not get to have an opinion about a shape it was not written for.
    let [a, b] = &sides[..] else {
        return Ok(());
    };
    for (n, sa) in a {
        let Some(sb) = b.get(n) else { continue };
        if sa == sb {
            continue;
        }
        return Err(misaligned(format!(
            "shape: ${n} is {} in {} but {} in {}",
            sa.label(),
            Origin::query(0).label(),
            sb.label(),
            Origin::query(1).label()
        )));
    }
    Ok(())
}

/// What kind of value one query wants at one `$N`. See [`check_shape`] for what establishes each.
#[derive(PartialEq, Eq, PartialOrd, Ord, Clone, Copy, Debug)]
enum Shape {
    /// An element of an `INSERT`'s `VALUES` row.
    RowValue,
    /// A positional argument of `unnest(…)`, or the operand of a cast to an array type.
    Array,
}

impl Shape {
    fn label(self) -> &'static str {
        match self {
            Shape::RowValue => "a row value",
            Shape::Array => "an array",
        }
    }
}

/// Every `$N` in one statement whose shape this walk can read unambiguously.
///
/// A parameter found in both roles is dropped rather than resolved: see [`check_shape`].
fn shapes_of(st: &Statement) -> BTreeMap<u32, Shape> {
    let mut found: BTreeMap<u32, BTreeSet<Shape>> = BTreeMap::new();
    for (n, s) in row_values(st).into_iter().map(|n| (n, Shape::RowValue)).chain(arrays(st)) {
        found.entry(n).or_default().insert(s);
    }
    found
        .into_iter()
        .filter_map(|(n, s)| match &s.iter().copied().collect::<Vec<_>>()[..] {
            [one] => Some((n, *one)),
            _ => None,
        })
        .collect()
}

/// The parameters that are elements of an `INSERT`'s `VALUES` rows.
fn row_values(st: &Statement) -> Vec<u32> {
    let mut body = match st {
        Statement::Insert(i) => match i.source.as_deref() {
            Some(q) => &*q.body,
            None => return Vec::new(),
        },
        // `WITH … INSERT`, which sqlparser hands back as a `Query` wrapping the statement.
        Statement::Query(q) => match &*q.body {
            SetExpr::Insert(Statement::Insert(i)) => match i.source.as_deref() {
                Some(s) => &*s.body,
                None => return Vec::new(),
            },
            _ => return Vec::new(),
        },
        _ => return Vec::new(),
    };
    // `INSERT INTO t (a, b) (VALUES (1, 2))` parses its parenthesized source as a nested query.
    while let SetExpr::Query(q) = body {
        body = &*q.body;
    }
    let SetExpr::Values(vals) = body else {
        return Vec::new();
    };
    vals.rows
        .iter()
        .flat_map(|r| r.content.iter())
        // `VALUES ($1::text[])` states an array outright, and `param_under_casts` answers `None` for
        // it: `arrays` records it, and calling it a row value here as well would only make the
        // parameter ambiguous and drop it.
        .filter_map(param_under_casts)
        .collect()
}

/// The parameters this statement uses as an array: `unnest($N)`, or `$N::T[]`.
///
/// Both hooks are needed, and this is the one place the walk cannot be an expression sweep. The shape
/// that matters most, `FROM unnest($1, $2)`, is a [`TableFactor`] and not an [`Expr::Function`] at all
/// — the `unnest` node simply does not exist in the expression tree, so only the parameters under it
/// are visited and nothing says what they are arguments to. `unnest($1::t[])` in the same position is
/// caught anyway, by the cast; `unnest($1)` has no cast and is caught only here. (An
/// [`Expr::Function`] hook is still carried, because `SELECT unnest($1)` puts the call in the select
/// list, where it is an expression.)
fn arrays(st: &Statement) -> Vec<(u32, Shape)> {
    let mut v = Arrays(Vec::new());
    let _ = st.visit(&mut v);
    v.0
}

/// [`arrays`]' walk. See it for why a [`TableFactor`] hook is not optional here.
struct Arrays(Vec<(u32, Shape)>);

impl Arrays {
    fn note<'a>(&mut self, args: impl IntoIterator<Item = &'a Expr>) {
        let found = args.into_iter().filter_map(param_under_casts);
        self.0.extend(found.map(|n| (n, Shape::Array)));
    }
}

impl Visitor for Arrays {
    type Break = ();

    fn pre_visit_table_factor(&mut self, tf: &TableFactor) -> ControlFlow<Self::Break> {
        match tf {
            TableFactor::UNNEST { array_exprs, .. } => self.note(array_exprs),
            TableFactor::Function { name, args, .. } if is_unnest(name) => {
                let args: Vec<&Expr> = args.iter().filter_map(unnamed).collect();
                self.note(args);
            }
            _ => {}
        }
        ControlFlow::Continue(())
    }

    fn pre_visit_expr(&mut self, e: &Expr) -> ControlFlow<Self::Break> {
        match e {
            Expr::Function(f) if f.over.is_none() && is_unnest(&f.name) => {
                if let FunctionArguments::List(l) = &f.args {
                    let args: Vec<&Expr> = l.args.iter().filter_map(unnamed).collect();
                    self.note(args);
                }
            }
            Expr::Cast { expr, .. } if matches!(scalar_cast_target(e), CastTarget::Array) => {
                self.note(std::iter::once(&**expr));
            }
            _ => {}
        }
        ControlFlow::Continue(())
    }
}

fn is_unnest(name: &ObjectName) -> bool {
    obj_name(name).eq_ignore_ascii_case("unnest")
}

/// A call's positional argument, or `None` for `*` and the named forms.
fn unnamed(a: &FunctionArg) -> Option<&Expr> {
    match a {
        FunctionArg::Unnamed(FunctionArgExpr::Expr(x)) => Some(x),
        _ => None,
    }
}

/// The parameter an expression *is*, seen through any number of non-array casts.
fn param_under_casts(e: &Expr) -> Option<u32> {
    match scalar_cast_target(e) {
        CastTarget::Scalar(inner) => param_index(inner),
        CastTarget::Array => None,
    }
}

/// Whether an expression is a cast to an array type, and what is underneath any casts that are not.
///
/// `$1::text::varchar` is still a scalar parameter, and `unnest($1::text[])` still names `$1`, so the
/// casts have to be read through rather than matched at one level. The array answer wins as soon as
/// one array cast is seen, on the way down: `$1::text[]::text[][]` is an array either way, and
/// `ARRAY[$1]::text[]` never reaches a parameter here at all.
fn scalar_cast_target(e: &Expr) -> CastTarget<'_> {
    let mut cur = unwrap_nested(e);
    loop {
        let Expr::Cast { expr, data_type, format, .. } = cur else {
            return CastTarget::Scalar(cur);
        };
        // The two spellings `normalize::distribute_array_casts` also declines to read: `CAST(x AS t
        // FORMAT f)` and the SQL-standard `t ARRAY`.
        if format.is_some() || matches!(data_type, DataType::Array(ArrayElemTypeDef::Qualified(..))) {
            return CastTarget::Scalar(cur);
        }
        if array_elem_type(data_type).is_some() {
            return CastTarget::Array;
        }
        cur = unwrap_nested(expr);
    }
}

/// [`scalar_cast_target`]'s answer.
enum CastTarget<'a> {
    /// A cast to an array type was found on the way down.
    Array,
    /// No array cast; the expression under any scalar casts.
    Scalar(&'a Expr),
}

/// Which of two refusals a misaligned pair reports, when the other one is inference failing.
///
/// The two are not independent. Identifying `$k` across two queries that number their placeholders
/// differently puts unrelated values in one type class, and the `type conflict` that comes out of
/// [`crate::infer`] is then something this frontend's own assumption manufactured, not something
/// inference found. Reporting it sends the caller to look at a type that is not the problem; the
/// numbering is.
///
/// **The rule: a misalignment outranks any refusal it could have manufactured, and yields to any
/// refusal it could not.** The counterfactual that decides which is which is
/// [`crate::infer::infers_with_split_params`] — would the pair have typed if the two queries' `$N` were
/// not identified? If yes, `arity` is the root cause and is reported. If no, the pair has a conflict of
/// its own, which is reported exactly as it was before this rule existed.
///
/// Nothing about a *verdict* turns on the choice: `arity` fired, so the pair is refused either way, and
/// the refused set is identical whichever reason is shown. What changes is which fact the caller is
/// handed first, and — because the report buckets by reason — which bucket the row is counted in.
///
/// `failure` is consumed rather than borrowed because exactly one of the two errors survives this call,
/// and handing back a reference to the loser would only invite a caller to report both.
pub fn root_cause(
    arity: Option<FrontendError>,
    failure: FrontendError,
    queries: &[Query],
    prov: Option<&Catalog>,
) -> FrontendError {
    match arity {
        Some(m) if infers_with_split_params(queries, prov) => m,
        _ => failure,
    }
}

/// [`root_cause`]'s rule at the second gate: which of two refusals a misaligned pair reports when the
/// other one is the pair failing to *lower*.
///
/// Most lowering refusals are about a construct, and no renumbering invents or removes a construct — so
/// this yields, and the row reports what it always did. The exception is every refusal that reads an
/// inferred *type*, because the type is what index binding forced. The clearest is the `LIMIT`/`OFFSET`
/// count guard (`lower.rs`'s `count`): one observed pair had `is_deleted = $4 LIMIT $5` against
/// `is_deleted = false LIMIT $4`, so the identification puts a boolean filter value and a row count in
/// one type class, the count comes out `BOOLEAN`, and the guard refuses a count that is only the wrong
/// type because the two queries were numbered differently. That is the misalignment wearing a costume,
/// and the same argument [`root_cause`] makes about `type conflict` applies unchanged.
///
/// `lowers_split` is the counterfactual — the crate's `lowers_with_split_params` — taken as a closure
/// rather than called here because it needs the whole pipeline, which lives above this module. It is
/// deliberately a *re-run* and not a test of the failing guard: asking "does the pair lower once the
/// parameters are pulled apart" needs no list of which refusals are type-dependent, and cannot go stale
/// when the next one is added. Lazy, so the pairs with no verdict pending — nearly all of them — pay
/// nothing.
///
/// When the counterfactual comes back `false` the *original* `failure` is reported, not whatever the
/// split run refused with: the split run's messages name renumbered parameters that are not in the
/// caller's input, and it exists to answer a yes/no question, not to word an error.
pub fn root_cause_lowered(
    misaligned: Option<FrontendError>,
    failure: FrontendError,
    lowers_split: impl FnOnce() -> bool,
) -> FrontendError {
    match misaligned {
        Some(m) if lowers_split() => m,
        _ => failure,
    }
}
