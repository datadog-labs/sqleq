// Unless explicitly stated otherwise all files in this repository are licensed under the
// Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/).
// Copyright 2026-Present Datadog, Inc.

//! `sqleq-frontend` — read a SQL equivalence pair, and lower it to a prover's input IR.
//!
//! This crate parses a SQL equivalence pair, resolves names and types, and lowers it to the
//! `Relation`/`Expr` JSON both proving axes consume: the QED prover directly, and SQLSolver through
//! the IR bridge in [`sqlsolver`]. It replaces the legacy Python preprocessor + Java/Calcite parser
//! with a single Rust frontend (see `docs/DESIGN.md`). The third axis, `sqleq-fuzz`, does not read
//! IR at all — it runs the two queries against DuckDB and looks for a counterexample.
//!
//! ## Soundness
//!
//! The prover is sound: it only proves genuinely-equivalent pairs *given faithful IR*. So the one
//! way to introduce a false positive is to lower SQL unfaithfully. The frontend therefore **refuses**
//! (returns [`FrontendError`]) any construct it cannot lower faithfully — `LIMIT`/`OFFSET`, window
//! functions, correlated columns it can't resolve, etc. — rather than emitting best-effort IR. It
//! never panics or `exit`s on bad input.
//!
//! It assumes exactly one thing it cannot check: that `$N` on one side is the same application value
//! as `$N` on the other (`src/params.rs`). `docs/SOUNDNESS.md` is the full argument — what is
//! refused and why refusing is the right trade, and what that one assumption does and does not
//! license.
//!
//! ## Entry point
//!
//! [`lower_sql`] takes the preprocessor `.sql` format (optional `CREATE TABLE`s, optional
//! `declare ... function` lines, then exactly two queries) and returns the prover `Input` JSON. The
//! two may also be a pair of `DELETE`s or a pair of `UPDATE`s, which `dml` reduces to the queries
//! computing their effect before anything else runs.

mod catalog;
mod casts;
/// Public because it is an entry point: the `--csv` mode of the CLI reads a corpus row and lowers it
/// without going through the `.sql` intermediate format at all.
pub mod corpus;
mod dml;
mod error;
/// Selected on the shipping path by [`CatalogSource`], off by default.
mod infer;
mod lower;
mod normalize;
/// Where the crate's one unstated assumption is checked: `$N` on one side is `$N` on the other.
mod params;
/// Public because it is an entry point: it reads raw Postgres DDL -- possibly malformed, since a
/// captured schema is not a schema anyone wrote by hand -- into a `Catalog`, and reports per
/// statement what it could not read rather than dropping it silently.
pub mod pgddl;
mod scope;
/// Public because it is an entry point: the `--sqlsolver` mode of the CLI turns corpus rows into
/// work for the second (SQLSolver) prover, which reads SQL text rather than our IR.
pub mod sqlsolver;
mod types;
mod verify;

/// Frontend internals, for sqleq's own benchmark harness.
///
/// Not an entry point and not public API: these are implementation details of the lowering
/// pipeline, exposed behind an off-by-default feature so the harness can audit a corpus
/// without carrying a second copy of the frontend. Exempt from semver -- anything here may
/// change or disappear in a patch release. If you are not that harness, do not enable it.
#[cfg(feature = "internals")]
#[doc(hidden)]
pub mod internals {
    pub use crate::catalog::{obj_name, Catalog, SYSTEM_COLUMNS};
    pub use crate::infer::{declares, nid};

    /// Aliased rather than re-exported: `DIALECT` sits at the crate root, where making it
    /// `pub` would widen the default API even with this feature off.
    pub const DIALECT: sqlparser::dialect::PostgreSqlDialect = crate::DIALECT;

    /// A pair file's statements, parsed, and checked by `normalize::fix_precedence` for the
    /// precedence shapes where sqlparser's tree is not Postgres's (an `Err` names the shape), and
    /// nothing else: no other refusal, no reduction, no rewrite. `declare ... function` lines are
    /// dropped.
    ///
    /// This is the tree *before* `params::check_shape` and `dml::reduce`, which both refuse the
    /// `INSERT ... VALUES` vs `INSERT ... SELECT * FROM unnest(..)` pairs that `sqleq-lean` exists
    /// to decide.
    pub fn parse_pair(src: &str) -> crate::Result<Vec<sqlparser::ast::Statement>> {
        let (_, mut statements) = crate::parse_statements(src)?;
        crate::normalize::fix_precedence(&mut statements)?;
        Ok(statements)
    }
}

use std::collections::{BTreeSet, HashMap};

use serde_json::{json, Value};
use sqlparser::dialect::PostgreSqlDialect;
use sqlparser::parser::Parser;

use catalog::{parse_declare, FnDecl};
pub use error::{FrontendError, Result};

/// The dialect every parse in this crate goes through, named once because the choice is load-bearing
/// rather than a default.
///
/// sqlparser's `GenericDialect` has no precedence for the Postgres JSON operators — `->`, `->>`, `#>`,
/// `#>>` all come back from `prec_unknown()` — so it parses `payload ->> 'k' = 'v'` as
/// `payload ->> ('k' = 'v')`. That is a *different predicate* than the query states, and
/// [`normalize::demote_operators`] would then faithfully lower the wrong tree. `PostgreSqlDialect`
/// assigns them `PG_OTHER_PREC`, above `=`, which is the Postgres grammar.
///
/// This is the same failure mode as the `IS DISTINCT FROM` bug in [`normalize`], and the two are fixed
/// differently for a reason: that one is in `parse_infix` and is there in every dialect, so it has to
/// be repaired in the tree, while this one *is* the dialect and is fixed by naming the right one.
///
/// `normalize`'s tests parse through this constant so the precedence it buys is pinned by a test
/// rather than assumed.
pub(crate) const DIALECT: PostgreSqlDialect = PostgreSqlDialect {};

/// Where the base-table schema comes from.
///
/// The frontend reads types off a `catalog::Catalog`; this picks which one it gets. Two of the
/// three run inference, and the reason that can be sound at all is worth restating here rather than
/// leaving it in `infer`: an inferred type is applied **identically to both queries of the pair**,
/// and the prover proves equivalence *relative to the schema it is handed*. A wrong guess therefore
/// answers a question about a different schema than the pair came from — it cannot make the prover
/// agree to a false equivalence over the schema it was given. What it can do is make the question
/// uninteresting, which is why inference prefers hard evidence to soft and yields an uninterpreted
/// sort rather than a plausible-looking `INTEGER` when it has none.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum CatalogSource {
    /// Read the input's `CREATE TABLE`s and nothing else. The default, settled by measuring what
    /// [`CatalogSource::Inferred`] costs: it gains cases, but nearly all of them are pairs whose two
    /// queries are textually identical, and it gives up real proofs in exchange.
    ///
    /// Note what this mode does to `$N`. The substitution that turns a placeholder into the nullary
    /// constant `qpN(0)` runs only under the inference modes (`lower_inner`, inside `infers()`), so
    /// here every parameterized row refuses on `literal Placeholder($N)`. On real rewrite pairs,
    /// where parameters are the norm rather than the exception, that leaves very little lowered — so
    /// a batch run uses `--infer-seeded`, and a figure measured under one mode says nothing at all
    /// about the other.
    #[default]
    Declared,
    /// Infer with the declared types as evidence, then lower against the *declared* catalog.
    ///
    /// Schema emission is identical to [`CatalogSource::Declared`] by construction, so what this
    /// adds is the cast rules and the `DECLARE` synthesis — the stages that decide parameter and
    /// unknown-function types, which no DDL declares. On a corpus whose rows carry DDL this is the
    /// production-shaped mode: declared columns, inferred parameters.
    InferredSeeded,
    /// Infer from the queries alone and lower against the synthesized catalog.
    ///
    /// The synthesized catalog holds only the columns the queries actually read, with no keys and
    /// everything nullable, so it does not line up with a declared one column-for-column. This is
    /// the mode for input that has no DDL at all.
    Inferred,
}

impl CatalogSource {
    fn infers(self) -> bool {
        self != CatalogSource::Declared
    }

    fn seeds_declared(self) -> bool {
        self != CatalogSource::Inferred
    }
}

/// Lower a two-query input into the prover's `Input` JSON, reading the declared `CREATE TABLE`s.
///
/// The input is the preprocessor `.sql` format: any `CREATE TABLE`s, any
/// `declare {scalar,aggregate} function NAME(args) returns TYPE;` lines (a custom DSL, stripped
/// before SQL parsing), and exactly two `SELECT` statements (the pair to compare).
pub fn lower_sql(src: &str) -> Result<Value> {
    lower_with(src, CatalogSource::Declared)
}

/// Lower a two-query input, choosing where the schema comes from. See [`CatalogSource`].
pub fn lower_with(src: &str, source: CatalogSource) -> Result<Value> {
    lower_inner(src, None, source)
}

/// Lower a pair against a catalog read from **raw Postgres DDL**, ignoring any `CREATE TABLE`s in
/// `src`.
///
/// This is the production shape: the schema comes from the DDL the caller actually has, not from a
/// preprocessor's translation of it into the five type names the prover understands. See `pgddl`
/// for what that translation was costing — most visibly every `NOT NULL`, which the preprocessor
/// discards outright.
///
/// A `declared` catalog is still the *declared* catalog for [`CatalogSource`]'s purposes, so all
/// three modes compose with this as they do with [`lower_with`].
pub fn lower_with_ddl(src: &str, ddl: &str, source: CatalogSource) -> Result<Value> {
    lower_inner(src, Some(pgddl::parse_provided_schema(ddl)), source)
}

fn lower_inner(
    src: &str,
    ddl_catalog: Option<catalog::Catalog>,
    source: CatalogSource,
) -> Result<Value> {
    // 1-2. Split out the `declare ... function` DSL lines, parse, build the declared catalog, reduce
    //      the DML, collect the two queries.
    let (fns, declared, queries, seen) = parse_input(src, ddl_catalog)?;
    // 3-5. Infer, apply what inference concluded, and lower.
    let (input, misaligned) = pipeline(queries, &fns, &declared, source, Align::Check(&seen))?;
    // 6. The pair lowers, so anything left is a statement about the *question* rather than about a
    //    construct: the two queries' `$N` do not correspond, and index binding is not what the caller
    //    means. Raised here so the reason counts only rows nothing else refused.
    if let Some(e) = misaligned {
        return Err(e);
    }
    Ok(input)
}

/// Whether a run performs the parameter-alignment checks of [`params`].
#[derive(PartialEq, Eq, Clone, Copy)]
enum Align<'a> {
    /// The real run: check, hold the verdict, hand it back for [`lower_inner`] to raise last.
    ///
    /// Carries the `$N` sets [`params::mentioned`] read off the pair **before** the normalizations, which
    /// is the only place they are still the caller's. The variant holds them rather than the pipeline
    /// recomputing them because a stage that deletes a placeholder — `strip_identical_pagination`, in the
    /// shape [`params::mentioned`] documents — otherwise makes the frontend refuse a pair for a
    /// misalignment it created itself.
    Check(&'a [BTreeSet<u32>]),
    /// A counterfactual run from [`lowers_with_split_params`], whose entire question is what the *rest*
    /// of the pipeline does once the two queries' parameters have been pulled apart.
    ///
    /// Checking again would be pointless — a split pair's two `$N` sets are disjoint by construction, so
    /// neither sub-reason can fire — and skipping it bounds the recursion at two runs *structurally*
    /// rather than on that argument. A future `params` that refused disjoint sets as well (the broad rule
    /// its module docs weigh and reject) would otherwise re-enter the counterfactual on its own output
    /// without end.
    Skip,
}

/// Steps 3-5: optionally infer, apply the stages that depend on what inference concluded (the cast
/// rules, the `$N -> qpN(0)` substitution, the `DECLARE` synthesis), then lower both queries.
///
/// Its own function because some pairs run it twice. [`lowers_with_split_params`] re-runs it on the
/// same pair with the two queries' parameters renumbered apart, which is how
/// [`params::root_cause_lowered`] tells a refusal the frontend's index binding *manufactured* from one
/// the pair has on its own.
///
/// The alignment verdict is returned rather than raised, because it is raised last — after lowering has
/// had its say. See [`params::check_arity`].
fn pipeline(
    mut queries: Vec<sqlparser::ast::Query>,
    fns: &HashMap<String, FnDecl>,
    declared: &catalog::Catalog,
    source: CatalogSource,
    align: Align,
) -> Result<(Value, Option<FrontendError>)> {
    let mut decls = fns.clone();
    let inferred;
    // Held rather than raised: `params` explains why this one refusal is reported last.
    let mut misaligned = None;
    // Every stage below rewrites `queries` in place, and the counterfactual at the bottom has to start
    // from the tree this run started from — so the tree is copied while it still exists. One copy per
    // pair, only in the modes that can reach a verdict at all, and never in a counterfactual's own run.
    let mut pristine = Vec::new();
    let catalog = if source.infers() {
        let seeds = source.seeds_declared().then_some(declared);
        let arity = if let Align::Check(seen) = align {
            pristine = queries.clone();
            // Syntactic, so it has a verdict even on a pair inference is about to reject — which is
            // the point: identifying `$k` across two queries that number their placeholders
            // differently is itself capable of producing that rejection. See `params::root_cause`.
            params::check_arity(seen).err()
        } else {
            None
        };
        let mut inf = match infer::infer(&queries, seeds) {
            Ok(inf) => inf,
            Err(e) => return Err(params::root_cause(arity, e, &queries, seeds)),
        };
        let rw = casts::rewrite_casts(&mut queries, &mut inf)?;
        // The only window where both facts hold: rule 1 has hoisted `$N::T` to a bare `$N`, and the
        // substitution below has not yet deleted the attribution the role check reads. `arity` first,
        // the order the two sub-reasons have always had.
        if matches!(align, Align::Check(_)) {
            misaligned = arity.or_else(|| params::check_roles(&queries, &inf).err());
        }
        casts::substitute_params(&mut queries, &mut inf)?;
        let mut synth = casts::declarations(&queries, &mut inf, &rw)?;
        // A `declare` line in the input still wins: it is a statement about the pair that inference
        // is in no position to overrule.
        synth.extend(decls);
        decls = synth;
        inferred = inf.catalog;
        if source.seeds_declared() {
            declared
        } else {
            &inferred
        }
    } else {
        declared
    };

    match emit(catalog, &decls, &queries) {
        Ok(input) => Ok((input, misaligned)),
        // The pair does not lower. A refusal this misalignment could have manufactured yields to it; a
        // refusal it could not have is what the row reports. See `params::root_cause_lowered`.
        Err(e) => Err(params::root_cause_lowered(misaligned, e, || {
            lowers_with_split_params(&pristine, fns, declared, source)
        })),
    }
}

/// Steps 4-5: the schemas both queries are lowered against, the two lowered queries, and the check
/// that the result numbers its variables the way the prover reads them.
fn emit(
    catalog: &catalog::Catalog,
    decls: &HashMap<String, FnDecl>,
    queries: &[sqlparser::ast::Query],
) -> Result<Value> {
    let schemas: Vec<Value> = catalog
        .tables
        .iter()
        .map(|t| {
            json!({
                // The prover addresses tables positionally -- `{"scan": i}` indexes this array -- so
                // it never reads the name. The SQLSolver bridge does: Calcite's root schema is
                // name-keyed, so `RelBuilder::scan` needs the spelling that `emit_mysql` printed into
                // the DDL. Emitting it here rather than alongside keeps one description of a table.
                //
                // Free of consequence for the qed axis: `Schema` derives a plain `Deserialize` with
                // no `deny_unknown_fields` (checked), so QED discards the field without noticing it.
                "name": t.name.clone(),
                "types": t.cols.iter().map(|(_, ty)| ty.clone()).collect::<Vec<_>>(),
                "key": t.keys.clone(),
                "nullable": t.nullable.clone(),
                "guaranteed": Vec::<Value>::new(),
            })
        })
        .collect();
    let q0 = lower::lower_query(catalog, decls, &queries[0])?;
    let q1 = lower::lower_query(catalog, decls, &queries[1])?;

    let mut input = json!({ "schemas": schemas, "queries": [q0, q1], "help": ["", ""] });
    // Nothing downstream re-checks the variable numbering, and getting it wrong yields a proof about
    // the wrong query rather than an error. See [`verify`].
    verify::check_levels(&input)?;
    types::rename_emitted_types(&mut input);
    Ok(input)
}

/// Would the pair have lowered if the two queries' `$N` were not identified?
///
/// The counterfactual behind [`params::root_cause_lowered`], and the lowering-gate twin of
/// [`infer::infers_with_split_params`]: renumber the second query's parameters clear of the first's and
/// run the same stages again. `true` means every refusal on the way down was one that identification
/// produced — a real re-run, so a construct the frontend cannot lower fires just the same and answers
/// `false`.
///
/// `queries` is the pair as it was *before* [`pipeline`] rewrote anything, so the pair-level
/// normalizations in [`parse_input`] — which ran under the original numbering, and which the split must
/// not be allowed to reconsider — are already baked in.
fn lowers_with_split_params(
    queries: &[sqlparser::ast::Query],
    fns: &HashMap<String, FnDecl>,
    declared: &catalog::Catalog,
    source: CatalogSource,
) -> bool {
    match infer::split_params(queries) {
        Some(split) => pipeline(split, fns, declared, source, Align::Skip).is_ok(),
        None => false,
    }
}

/// What [`parse_input`] hands back: the declared functions, the catalog, the two queries, and the `$N`
/// each of them mentioned *before* the normalizations ran.
///
/// The last is separate from the queries on purpose — see [`params::mentioned`] — because by the time the
/// pipeline has the trees, a strip may already have deleted a placeholder from one side.
type ParsedInput =
    (HashMap<String, FnDecl>, catalog::Catalog, Vec<sqlparser::ast::Query>, Vec<BTreeSet<u32>>);

/// Split the preprocessor `.sql` format into its function declarations, its declared catalog and its
/// two queries. The `declare ... function` lines are a custom DSL, not SQL, so they come out first.
///
/// `ddl_catalog` is the caller's own schema, from [`lower_with_ddl`]; when it is given, the input's own
/// `CREATE TABLE`s are ignored. It is resolved *here* rather than by the caller because the DML
/// reduction needs the catalog the pair will actually be lowered against, and it runs inside this
/// function.
/// The head of [`parse_input`]: the `declare ... function` DSL lines split off, the rest parsed.
///
/// Its own function because [`reflexive`] needs exactly this much and nothing below it. Two copies
/// of the split would be two things to keep in step, and the whole point of the reflexivity check is
/// that it sees the same tree the shipping path sees.
fn parse_statements(src: &str) -> Result<(HashMap<String, FnDecl>, Vec<sqlparser::ast::Statement>)> {
    let mut fns: HashMap<String, FnDecl> = HashMap::new();
    let mut sql_lines: Vec<&str> = Vec::new();
    for line in src.lines() {
        let t = line.trim_start().to_lowercase();
        if t.starts_with("declare ") && t.contains("function") {
            if let Some((name, ret)) = parse_declare(line) {
                fns.insert(name, ret);
            }
        } else {
            sql_lines.push(line);
        }
    }
    let sql = sql_lines.join("\n");
    // The default nesting limit (50) is below what generated SQL reaches; the parser's own recursion
    // is stack-protected, and the lowering walks an `AND`/`OR` chain iteratively.
    let statements = Parser::new(&DIALECT)
        .with_recursion_limit(1024)
        .try_with_sql(&sql)
        .and_then(|mut p| p.parse_statements())
        .map_err(|e| FrontendError::Parse(e.to_string()))?;
    Ok((fns, statements))
}

/// Which of [`reflexive_with`]'s normalizations are switched on.
///
/// Exists for one reason: attribution. A reflexive verdict is only as strong as the rewrites it
/// rests on, and "which rewrite closed this pair" is not answerable from the outside — so a caller
/// re-runs the check with one bit cleared at a time and reads off the difference.
/// [`Rewrites::ALL`] is what [`reflexive`] uses and the only combination that ships.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Rewrites(u16);

impl Rewrites {
    pub const FIX_PRECEDENCE: Rewrites = Rewrites(1 << 0);
    pub const DEMOTE_OPERATORS: Rewrites = Rewrites(1 << 1);
    pub const STRIP_IN_EXISTS_DISTINCT: Rewrites = Rewrites(1 << 2);
    pub const UNNEST_IN_TO_ANY: Rewrites = Rewrites(1 << 3);
    pub const INLINE_CTES: Rewrites = Rewrites(1 << 4);
    pub const STRIP_IDENTICAL_PAGINATION: Rewrites = Rewrites(1 << 5);
    pub const STRIP_DEAD_ORDER_BY: Rewrites = Rewrites(1 << 6);
    pub const STRIP_SCHEMA: Rewrites = Rewrites(1 << 7);
    pub const DISTRIBUTE_ARRAY_CAST: Rewrites = Rewrites(1 << 8);
    pub const STRIP_IDENTICAL_LOCKS: Rewrites = Rewrites(1 << 9);
    /// The ten bits above and exactly those. Not `u16::MAX`: a bit [`Rewrites::EACH`] does not name
    /// is a rewrite an attribution pass reports as "no rewrite was necessary" for every row it
    /// closes, so the two are pinned equal by a test and this mask is what makes that pin possible.
    pub const ALL: Rewrites = Rewrites((1 << 10) - 1);
    /// Every rewrite except one — the single-subtraction counterfactual.
    pub const NONE: Rewrites = Rewrites(0);

    pub fn without(self, other: Rewrites) -> Rewrites {
        Rewrites(self.0 & !other.0)
    }

    pub fn with(self, other: Rewrites) -> Rewrites {
        Rewrites(self.0 | other.0)
    }

    fn has(self, other: Rewrites) -> bool {
        self.0 & other.0 != 0
    }

    /// The individual rewrites, in the order [`reflexive_with`] runs them, each with the stable name
    /// an attribution pass reports it under.
    pub const EACH: [(&'static str, Rewrites); 10] = [
        ("fix_precedence", Rewrites::FIX_PRECEDENCE),
        ("demote_operators", Rewrites::DEMOTE_OPERATORS),
        ("strip_in_exists_distinct", Rewrites::STRIP_IN_EXISTS_DISTINCT),
        ("unnest_in_to_any", Rewrites::UNNEST_IN_TO_ANY),
        ("distribute_array_casts", Rewrites::DISTRIBUTE_ARRAY_CAST),
        ("inline_ctes", Rewrites::INLINE_CTES),
        ("strip_identical_locks", Rewrites::STRIP_IDENTICAL_LOCKS),
        ("strip_identical_pagination", Rewrites::STRIP_IDENTICAL_PAGINATION),
        ("strip_dead_order_by", Rewrites::STRIP_DEAD_ORDER_BY),
        ("strip_schema", Rewrites::STRIP_SCHEMA),
    ];
}

/// Whether the pair's two sides become the **same tree** under this crate's normalizations — so the
/// row is settled by inspection, with no schema, no types and no prover.
///
/// ## Why this exists
///
/// The frontend refuses on the *first* construct it cannot lower, and that refusal happens before
/// anything notices the two sides had already collapsed into one. A pair whose only difference is an
/// inserted `DISTINCT` — a difference [`normalize::strip_in_exists_distinct`] erases — is therefore
/// reported as unhandled because of some unrelated construct further down the query. The row is
/// answered; the answer is just not reached.
///
/// This is not a second lowering path and it cannot become one: it returns a `bool`, it emits no IR,
/// and every way it can fail returns `false`. It can only turn a refusal into "the two sides are the
/// same query", never into a proof about two different ones.
///
/// ## Why it is sound without a catalog
///
/// Every normalization it runs is purely syntactic — none of them takes a [`catalog::Catalog`]. So
/// each one's existing equivalence argument, and each one's guards, carry over unchanged to pairs
/// this crate cannot type-check: `q ≡ q` needs no schema. Two consequences worth stating rather than
/// leaving implicit:
///
/// * A verdict here is exactly as strong as the weakest normalization it rests on. Two are worth
///   naming: [`normalize::unnest_in_to_any`], whose claim is NULL-vs-FALSE in filter position rather
///   than exact equality, and [`normalize::strip_dead_order_by`], which asserts an ordering is
///   unobservable. Both already ship on the lowering path, so this widens their reach without adding
///   a new kind of risk.
/// * Nondeterminism is *not* a hazard. If both sides normalize to one tree they are one query, and a
///   query is equivalent to itself however many `now()`s it contains.
///
/// Structural equality is the right comparison because sqlparser's `PartialEq` is span-insensitive:
/// `Ident` destructures with `span: _`, `ValueWithSpan` compares only `.value`, and `AttachedToken`
/// compares equal unconditionally.
///
/// ## What it deliberately skips
///
/// [`dml::reduce`], which is the only stage here that would need the catalog — and the stage that
/// raises on `UPDATE ... RETURNING`, so skipping it is what makes a DML pair reachable at all. For
/// those the comparison is on the unreduced statements, which is the simpler claim anyway: two
/// identical `UPDATE`s are the same statement, no reduction required.
// The prose above cites the normalizations it depends on by name; those live in private
// modules, so the links only resolve under `--document-private-items`.
#[allow(rustdoc::private_intra_doc_links)]
pub fn reflexive(src: &str) -> bool {
    reflexive_with(src, Rewrites::ALL)
}

/// [`reflexive`] with an explicit set of normalizations; see [`Rewrites`].
pub fn reflexive_with(src: &str, rewrites: Rewrites) -> bool {
    match normalized_pair(src, rewrites) {
        Some((a, b)) => a == b,
        None => false,
    }
}

/// The two sides as [`reflexive_with`] compares them, rendered back to SQL. For reading hits by
/// hand: a `bool` says a pair collapsed but not to what, and an attribution that surprises you is
/// only answerable by looking at the tree.
pub fn reflexive_forms(src: &str, rewrites: Rewrites) -> Option<(String, String)> {
    normalized_pair(src, rewrites).map(|(a, b)| (a.to_string(), b.to_string()))
}

/// The pair under `rewrites`, or `None` when the input is not a pair at all. Splitting this out of
/// [`reflexive_with`] keeps the comparison and the rendering reading the very same tree.
fn normalized_pair(
    src: &str,
    rewrites: Rewrites,
) -> Option<(sqlparser::ast::Statement, sqlparser::ast::Statement)> {
    let Ok((_, mut statements)) = parse_statements(src) else {
        return None;
    };
    // A repair of a parser bug rather than a rewrite, so it runs here for the same reason it runs
    // first in `parse_input`: everything below reads the tree it produces.
    if rewrites.has(Rewrites::FIX_PRECEDENCE)
        && normalize::fix_precedence(&mut statements).is_err()
    {
        return None;
    }
    if rewrites.has(Rewrites::DEMOTE_OPERATORS) {
        normalize::demote_operators(&mut statements);
    }
    if rewrites.has(Rewrites::STRIP_IN_EXISTS_DISTINCT) {
        normalize::strip_in_exists_distinct(&mut statements);
    }
    if rewrites.has(Rewrites::UNNEST_IN_TO_ANY) {
        normalize::unnest_in_to_any(&mut statements);
    }
    if rewrites.has(Rewrites::DISTRIBUTE_ARRAY_CAST) {
        normalize::distribute_array_casts(&mut statements);
    }

    // The pair, without the `CREATE TABLE`s `catalog::scan_ddl` reads. Anything but exactly two is
    // not a pair, whatever else it is.
    let rest: Vec<sqlparser::ast::Statement> = statements
        .into_iter()
        .filter(|st| !matches!(st, sqlparser::ast::Statement::CreateTable(_)))
        .collect();
    if rest.len() != 2 {
        return None;
    }
    if !rest.iter().all(|st| matches!(st, sqlparser::ast::Statement::Query(_))) {
        let mut it = rest.into_iter();
        return Some((it.next()?, it.next()?));
    }
    // Both sides are queries, so the query-level normalizations apply too — in `parse_input`'s order,
    // which two of them depend on (see the comments there).
    let mut queries: Vec<sqlparser::ast::Query> = rest
        .into_iter()
        .filter_map(|st| match st {
            sqlparser::ast::Statement::Query(q) => Some(*q),
            _ => None,
        })
        .collect();
    if rewrites.has(Rewrites::INLINE_CTES) {
        normalize::inline_ctes(&mut queries);
    }
    if rewrites.has(Rewrites::STRIP_IDENTICAL_LOCKS) {
        normalize::strip_identical_locks(&mut queries);
    }
    if rewrites.has(Rewrites::STRIP_IDENTICAL_PAGINATION) {
        normalize::strip_identical_pagination(&mut queries);
    }
    if rewrites.has(Rewrites::STRIP_DEAD_ORDER_BY) {
        normalize::strip_dead_order_by(&mut queries);
    }
    if rewrites.has(Rewrites::STRIP_SCHEMA) {
        normalize::strip_schema(&mut queries);
    }
    let mut it = queries.into_iter().map(|q| sqlparser::ast::Statement::Query(Box::new(q)));
    Some((it.next()?, it.next()?))
}

fn parse_input(
    src: &str,
    ddl_catalog: Option<catalog::Catalog>,
) -> Result<ParsedInput> {
    let (fns, mut statements) = parse_statements(src)?;
    // Before anything reads the tree: sqlparser mis-parses `IS [NOT] DISTINCT FROM`, and lowering
    // the mis-parse is a false-proof channel. See `normalize`.
    normalize::fix_precedence(&mut statements)?;
    // Before the DML reduction, and so before every refusal in it: a pair whose two halves want a row
    // value and an array at the same `$N` is not a question the reduction's capability has anything to
    // say about. The other two alignment sub-reasons are raised *last* instead;
    // `params::check_shape` argues the difference.
    params::check_shape(&statements)?;
    // The catalog next, because the DML reduction needs the target table's columns; and the reduction
    // before every rewrite below it, so that nothing downstream of here has to know DML exists.
    let mut catalog = ddl_catalog.unwrap_or_else(|| catalog::scan_ddl(&statements));
    catalog.check_case_collisions()?;
    dml::reduce(&catalog, &mut statements)?;
    // Then the normalizations, which are rewrites of a correct tree rather than repairs of a
    // wrong one — so each carries its equivalence argument and that argument's guards.
    //
    // Demotion first: it turns operators into `q_*` calls, which is the form inference and the
    // declaration synthesis downstream both expect to see.
    normalize::demote_operators(&mut statements);
    normalize::strip_in_exists_distinct(&mut statements);
    // After the `DISTINCT` strip above, so the subquery it matches has already been simplified, and
    // on statements rather than queries so it reaches a reduced `DELETE`'s `WHERE` (which `dml`
    // turns into a plain `SELECT ... WHERE`).
    normalize::unnest_in_to_any(&mut statements);
    // After `unnest_in_to_any`, which is what produces the `= ANY(ARRAY[..]::t[])` several of
    // the rows this reaches are written as `IN (SELECT unnest(..))`. Before `infer`, so the
    // element cast it leaves behind types the placeholder under it.
    normalize::distribute_array_casts(&mut statements);
    let mut queries = catalog::collect_queries(statements)?;
    // The `$N` each query mentions, read here and not later: the pair is decided (the DML is reduced,
    // the two queries separated) and no rewrite below has yet been able to delete a placeholder. Every
    // strip after this point can, and `params::mentioned` documents what that cost when the check read
    // the trees instead.
    let seen = params::mentioned(&queries);
    // Before `strip_schema`, which would otherwise turn `part_16.c` into something a CTE named `c`
    // captures.
    normalize::inline_ctes(&mut queries);
    // After `inline_ctes`, so a lock clause inside a `WITH` binding sits on the derived table that
    // replaced it and is compared in the position lowering will see it.
    normalize::strip_identical_locks(&mut queries);
    // Pair-level, so it needs both queries and runs after they are separated out. Before the
    // `ORDER BY` strip, which it can unblock: removing the pair's only `LIMIT` leaves an ordering
    // with nothing downstream to consume it.
    normalize::strip_identical_pagination(&mut queries);
    normalize::strip_dead_order_by(&mut queries);
    normalize::strip_schema(&mut queries);
    // Last, so it reads the trees lowering will actually see. After the DML reduction on purpose: an
    // `UPDATE`'s projection is the declared table shape, not the widened one.
    catalog::add_system_columns(&mut catalog, &queries);
    Ok((fns, catalog, queries, seen))
}
