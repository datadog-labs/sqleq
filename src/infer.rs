// Unless explicitly stated otherwise all files in this repository are licensed under the
// Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/).
// Copyright 2026-Present Datadog, Inc.

//! Type inference for queries whose tables are **not** declared.
//!
//! Everything else in this crate reads types off a declared catalog: `catalog::scan_ddl` turns the
//! input's `CREATE TABLE`s into [`Catalog`](crate::catalog::Catalog), `Scope::try_resolve` hands a column's type to
//! `lower.rs`, and `types.rs` maps and coerces types that are already known. None of it *decides*
//! what type an undeclared column has. This module makes that decision, and whether it can make it
//! is the single largest factor in whether a pair with no DDL can be lowered at all.
//!
//! It is a second producer of the same [`Catalog`](crate::catalog::Catalog) type, so nothing downstream changes:
//!
//! ```text
//!                   ┌─ declared:  scan_ddl(statements)           (today)
//!   Catalog  ───────┤
//!                   └─ inferred:  infer(queries, ..)             (here)
//! ```
//!
//! ## Why inference can be sound at all
//!
//! Guessing a column's type sounds like exactly the kind of best-effort the crate docs forbid. The
//! reason it is not: the inferred type is applied **identically to both queries of the pair**, and
//! the prover proves equivalence *relative to* the schema it is handed. A wrong guess therefore
//! answers a question about a different schema than the one the row came from — it does not make the
//! prover agree to a false equivalence over the schema it was given. What it can do is make the
//! question uninteresting, so the ranking below prefers hard evidence to soft, and anything with no
//! evidence at all becomes [`Ty::Opaque`](crate::infer::Ty::Opaque) rather than a plausible-looking `INTEGER`.
//!
//! ## Evidence ranking
//!
//! Every atom (a `(table, column)` pair, a parameter, an unknown function's result) collects typed
//! evidence, and evidence carries a confidence. Higher confidence wins; **equal confidence
//! disagreeing is a refusal**, not a coin flip. That last rule is what keeps the module honest: the
//! alternative is picking one and reporting nothing.
//!
//! A refusal names the atoms that disagreed and, for each type, which of the pair's queries argued
//! for it ([`Origin`](crate::infer::Origin)) — `type conflict INTEGER/BOOLEAN unifying dogs.id with $2` says far more than
//! `type conflict INTEGER/BOOLEAN` about where to look.
//!
//! **What [`Origin`](crate::infer::Origin) is not:** it does not say whether a disagreement is an artifact of one union-find
//! spanning both queries. Equal-and-agreeing evidence unions its provenance, so a class picks up both
//! queries as soon as a name guess in each query agrees with the DDL, and the origins then overlap
//! regardless of how the conflict actually arose. The fact that *would* answer that question lives on
//! the union edges — which query's predicate joined two classes — not on the type evidence. To ask it,
//! change the pair and infer again: [`infers_with_split_params`](crate::infer::infers_with_split_params) renumbers the second query's
//! parameters clear of the first's and reports whether the failure survives. That is the definition,
//! it costs one extra pass on a row that has already failed, and it is what
//! [`crate::params::root_cause`] uses to tell a conflict inference *found* from one a misalignment
//! *manufactured*.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::ops::ControlFlow;

use sqlparser::ast::{
    visit_expressions, visit_expressions_mut, AccessExpr, BinaryOperator, Expr, FunctionArg,
    FunctionArgExpr, FunctionArguments, Ident, ObjectName, Query, Select, TableFactor,
    TableWithJoins, Value as SqlValue, Visit, Visitor,
};

use crate::catalog::{obj_name, Catalog, Table};
use crate::dml::fold_ident;
use crate::error::{schema, unsupported, FrontendError, Result};

/// The types inference can conclude. A deliberately coarse lattice: the concrete points and a top.
///
/// The temporal points are kept apart from `Int` and from each other: a date counts days and a
/// timestamp counts microseconds, and reading both as one integer is unsound (see the module docs
/// of `types.rs`). They are peers in [`Uf::merge`], so a parameter compared with a DATE on one side
/// and a TIMESTAMP on the other is a type conflict, not a guess.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Ty {
    Int,
    Real,
    Str,
    Bool,
    Date,
    Time,
    Timestamp,
    TimestampTz,
    Interval,
    /// No confident type. Rendered as `VARBINARY`, which the prover treats as an uninterpreted
    /// `Custom` sort supporting `=` only: equality, `IN` and projection work, while any use that
    /// needs an order or arithmetic fails loudly instead of silently assuming one.
    Opaque,
}

impl Ty {
    /// The prover type string. Note `Real` is `DOUBLE`, matching what the preprocessor emits.
    pub fn sql(self) -> &'static str {
        match self {
            Ty::Int => "INTEGER",
            Ty::Real => "DOUBLE",
            Ty::Str => "VARCHAR",
            Ty::Bool => "BOOLEAN",
            Ty::Date => "DATE",
            Ty::Time => "TIME",
            Ty::Timestamp => "TIMESTAMP",
            Ty::TimestampTz => "TIMESTAMPTZ",
            Ty::Interval => "INTERVAL",
            Ty::Opaque => "VARBINARY",
        }
    }

    /// Read back a prover type string, as [`Catalog`] stores it.
    ///
    /// Total, unlike [`map_type_name`], and that difference is the point. This reads a type the
    /// frontend has *already* classified, so anything it does not name is an uninterpreted sort —
    /// which is what [`Ty::Opaque`] means. [`map_type_name`] reads a type name out of the query
    /// text, where a name nobody recognises is a construct we were asked to model and could not.
    /// Both `REAL` and `DOUBLE` appear: `types.rs` writes the former, the preprocessor the latter.
    pub fn from_prover(t: &str) -> Ty {
        match t.to_uppercase().as_str() {
            "INTEGER" => Ty::Int,
            "REAL" | "DOUBLE" => Ty::Real,
            "VARCHAR" => Ty::Str,
            "BOOLEAN" => Ty::Bool,
            "DATE" => Ty::Date,
            "TIME" => Ty::Time,
            "TIMESTAMP" => Ty::Timestamp,
            "TIMESTAMPTZ" => Ty::TimestampTz,
            "INTERVAL" => Ty::Interval,
            _ => Ty::Opaque,
        }
    }
}

/// Confidence in a piece of type evidence. Higher wins on conflict; equal-and-different refuses.
///
/// The ordering is the whole soundness argument of the module, so it is worth stating why each rank
/// sits where it does:
///
/// * [`Conf::Name`] — the column is *called* `user_id`, so it is probably an integer. A naming
///   convention, not a fact about the data. Weakest on purpose.
/// * [`Conf::Use`] — it appears as `x LIKE '%a%'`, or next to a string literal. Says what the query
///   assumes, which is better evidence than what someone named it.
/// * [`Conf::Cast`] — it appears as `x::integer`. The query states the type outright.
/// * [`Conf::Schema`] — a `CREATE TABLE` declares it. Not inference at all, and authoritative.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub enum Conf {
    Name = 1,
    Use = 2,
    Cast = 3,
    Schema = 4,
}

/// Which of the pair's queries argued for a piece of type evidence. Reported in refusals so a type
/// conflict says where its two halves came from.
///
/// A bitset rather than an index because agreeing evidence merges: once both queries have argued for
/// a class's type, neither one owns it.
///
/// **Not a cross-query test.** It is tempting to read disjoint origins as "this conflict exists only
/// because one union-find spans the pair", and that reading is wrong in both directions — see the
/// module docs. A real pair is the counterexample: a parameter renumbered across the rewrite, whose
/// conflict reports `queries A,B vs declared` because a name guess in each query agreed with the DDL.
#[derive(Clone, Copy, PartialEq, Eq, Default, Debug)]
pub struct Origin(u8);

impl Origin {
    /// Evidence belonging to no query: a `CREATE TABLE`.
    pub const DECLARED: Origin = Origin(0);

    /// Evidence from `queries[i]`. Saturates at eight; `parse_input` guarantees two.
    pub fn query(i: usize) -> Origin {
        Origin(1u8 << i.min(7))
    }

    /// What the pair's queries are called in a refusal: `A` and `B`, never an index, because `0` and
    /// `1` in a message about `$0`/`$1` read as parameter numbers.
    ///
    /// The one place the queries are named, so this module and `params` cannot drift apart on it.
    pub fn label(self) -> String {
        if self.0 == Origin::DECLARED.0 {
            return "declared".to_string();
        }
        let names: Vec<String> = (0..8)
            .filter(|i| self.0 & (1 << i) != 0)
            .map(|i| ((b'A' + i as u8) as char).to_string())
            .collect();
        if names.len() == 1 {
            format!("query {}", names[0])
        } else {
            format!("queries {}", names.join(","))
        }
    }
}

/// Something that can carry a type: a base-table column, a query parameter, or the result of a
/// function we do not model.
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub enum Atom {
    /// `(table, column)`, each named as Postgres identifies it: an unquoted name folded to lower
    /// case, a quoted one as written ([`crate::dml::fold_ident`]), so `"Orders"` and `orders` are
    /// two tables and `"createdAt"` is not `createdat`. A table is its folded parts joined by `.`
    /// ([`folded_name`]).
    Col(String, String),
    /// `$N` in the source; `qpN(0)` once the preprocessor has substituted it.
    Param(u32),
    /// An unknown function's result, keyed by the *identity* of the call node ([`nid`]). By identity
    /// rather than by text, so two syntactically identical calls stay distinct atoms — unifying them
    /// would assert that the same call in two places returns the same thing, which is true for a
    /// pure function but is not what the evidence says.
    FnResult(usize),
}

impl Atom {
    /// How the atom is named in a refusal: `$N` as the caller wrote it, not the `qpN(0)` spelling the
    /// substitution gives it later.
    pub fn label(&self) -> String {
        match self {
            Atom::Col(t, c) => format!("{t}.{c}"),
            Atom::Param(n) => format!("${n}"),
            // The key is a node identity, which would mean nothing to whoever reads the refusal.
            Atom::FnResult(_) => "a function result".to_string(),
        }
    }
}

/// Two pieces of evidence that disagree at equal confidence, with where each came from.
///
/// Returned by [`Uf::merge`] rather than formatted there, because only the caller knows what it was
/// doing when the disagreement surfaced: [`Uf::set_type`] has one atom to name, [`Uf::union`] has the
/// pair it was joining.
#[derive(Clone, Copy, Debug)]
pub struct Clash {
    pub a: Ty,
    pub b: Ty,
    pub from: (Origin, Origin),
}

impl Clash {
    /// `at` names the location in the caller's own words, e.g. `"at $2"`.
    fn err(&self, at: &str) -> FrontendError {
        let (x, y) = self.from;
        let from =
            if x == y { x.label() } else { format!("{} vs {}", x.label(), y.label()) };
        schema(format!("type conflict {}/{} {at} ({from})", self.a.sql(), self.b.sql()))
    }
}

/// Union-find over [`Atom`]s, carrying `(type, confidence, provenance)` per equivalence class.
///
/// Comparisons and `CASE` branches force two atoms to share a type without saying what it is, which
/// is why this is a union-find rather than a map: `WHERE a.x = b.y AND b.y::integer = 1` types
/// `a.x` through `b.y`.
#[derive(Default)]
pub struct Uf {
    parent: HashMap<Atom, Atom>,
    ty: HashMap<Atom, (Ty, Conf, Origin)>,
    /// Stamped onto every piece of evidence [`Uf::set_type`] introduces. Held here rather than passed
    /// per call because [`gather_types`] is driven one query at a time: the driver knows which query
    /// it is walking, so the two dozen call sites inside it do not have to carry it.
    origin: Origin,
}

impl Uf {
    pub fn new() -> Self {
        Self::default()
    }

    /// Attribute everything set from here on to `s`. See [`Origin`].
    pub fn on_query(&mut self, s: Origin) {
        self.origin = s;
    }

    fn find(&mut self, x: &Atom) -> Atom {
        let mut cur = x.clone();
        loop {
            let p = match self.parent.get(&cur) {
                Some(p) => p.clone(),
                None => {
                    self.parent.insert(cur.clone(), cur.clone());
                    return cur;
                }
            };
            if p == cur {
                return cur;
            }
            // Halve the path as we go (grandparent splicing), same as the Python version.
            if let Some(gp) = self.parent.get(&p).cloned() {
                self.parent.insert(cur.clone(), gp);
            }
            cur = p;
        }
    }

    /// Combine two pieces of evidence. Disagreement at equal confidence is a refusal: there is no
    /// principled tie-break, and picking one silently is how a wrong schema stops being visible.
    ///
    /// Agreement unions the provenance, because a type both queries argue for belongs to neither
    /// alone. When confidence decides it, the winner's provenance is the one that survives: the
    /// loser's type was discarded, so it has nothing left to vouch for.
    fn merge(
        a: Option<(Ty, Conf, Origin)>,
        b: Option<(Ty, Conf, Origin)>,
    ) -> std::result::Result<Option<(Ty, Conf, Origin)>, Clash> {
        match (a, b) {
            (None, x) | (x, None) => Ok(x),
            (Some((ta, ca, sa)), Some((tb, cb, sb))) => {
                if ta == tb {
                    Ok(Some((ta, ca.max(cb), Origin(sa.0 | sb.0))))
                } else if ca > cb {
                    Ok(Some((ta, ca, sa)))
                } else if cb > ca {
                    Ok(Some((tb, cb, sb)))
                } else {
                    Err(Clash { a: ta, b: tb, from: (sa, sb) })
                }
            }
        }
    }

    pub fn set_type(&mut self, x: &Atom, ty: Ty, conf: Conf) -> Result<()> {
        let r = self.find(x);
        let merged = Self::merge(self.ty.get(&r).copied(), Some((ty, conf, self.origin)))
            .map_err(|c| c.err(&format!("at {}", x.label())))?;
        if let Some(m) = merged {
            self.ty.insert(r, m);
        }
        Ok(())
    }

    pub fn union(&mut self, a: &Atom, b: &Atom) -> Result<()> {
        let (ra, rb) = (self.find(a), self.find(b));
        if ra == rb {
            return Ok(());
        }
        // Two classes already typed as different points of the temporal promotion chain are not a
        // conflict: `d < ts` is how Postgres compares a DATE with a TIMESTAMP, by promoting the date.
        // They stay separate classes, each keeping its type, and the lowering puts the conversion
        // between them (`types::coerce_cmp`). Merging would either refuse the pair or relabel one
        // column with the other's unit.
        let chain = |t: Ty| matches!(t, Ty::Date | Ty::Timestamp | Ty::TimestampTz);
        if let (Some((ta, ..)), Some((tb, ..))) = (self.ty.get(&ra), self.ty.get(&rb)) {
            if ta != tb && chain(*ta) && chain(*tb) {
                return Ok(());
            }
        }
        let merged = Self::merge(self.ty.get(&ra).copied(), self.ty.get(&rb).copied())
            .map_err(|c| c.err(&format!("unifying {} with {}", a.label(), b.label())))?;
        self.parent.insert(rb.clone(), ra.clone());
        self.ty.remove(&rb);
        if let Some(m) = merged {
            self.ty.insert(ra, m);
        }
        Ok(())
    }

    pub fn get_type(&mut self, x: &Atom) -> Option<Ty> {
        let r = self.find(x);
        self.ty.get(&r).map(|(t, _, _)| *t)
    }
}

// ---------------------------------------------------------------------------
// Postgres type name -> Ty
// ---------------------------------------------------------------------------

/// The normalized spelling of a type name: trimmed, unquoted, lower-cased.
///
/// One definition with a name, because two callers must agree on when two spellings are the same
/// type: [`map_type_name`] reads a [`Ty`] out of it, and the cast rewrite keys its `qcast` symbols on
/// it for the targets that have no `Ty`. If those normalizations drifted apart, two spellings of one
/// type would get two symbols and stop cancelling.
pub fn canon_type_name(txt: &str) -> String {
    txt.trim().replace(['"', '`'], "").to_lowercase()
}

/// A cast/column type name → [`Ty`], plus whether the target carried a length/precision qualifier.
///
/// `None` means unmappable — `jsonb`, `geometry`, an enum, an array, a float (`double precision`
/// rounds, and the IR's REAL is exact), `uuid` (its input reads `'{A0EE…}'` and `'a0ee…'` as one
/// value, which text does not), `citext` and `char(n)`. This function stays silent
/// rather than guess, and *inference* keeps that silence: an unmappable target contributes no type,
/// exactly as a column nobody told us about contributes none.
///
/// The cast rewrite is the one caller that does more, and the distinction is the reason it may.
/// What an unmappable target denies us is not the *value's* type but the *cast's* interpretation,
/// and an uninterpreted function into an equality-only sort states exactly that — so `casts::decide`
/// carries the target as [`Ty::Opaque`] and wraps the cast in a `qcast` symbol keyed on the target's
/// spelling. That carrier stays on the read side and is deliberately never written back into the
/// union-find: [`Uf::merge`] treats `Opaque` as a peer of every other type, so a written one would
/// clash with real evidence at equal confidence and refuse the pair for a different reason instead of
/// proving it.
///
/// The `qualified` flag exists for the identity-cast rule. `x::varchar` over a VARCHAR column is a
/// no-op and can be dropped; `x::varchar(8)` is a *truncation* and cannot.
///
/// Quotes are stripped first. A name a dialect does not recognise as a type comes back through
/// `sqlparser` as a user-defined one and prints the way it was written, so `CAST(x AS "bpchar")` has
/// to reach what `CAST(x AS bpchar)` reaches — a type is the same type whichever
/// way it was spelled, and treating the quoted form as unmappable would refuse the pair over
/// punctuation.
pub fn map_type_name(txt: &str) -> (Option<Ty>, bool) {
    let t = canon_type_name(txt);
    // `int ARRAY` and `int ARRAY[4]` are the SQL-standard spellings of `int[]`. Without the word test
    // the scalar mapping below would read them by their element type and they would come back as `Int`.
    let array_word = t.split_whitespace().any(|w| w == "array" || w.starts_with("array["));
    if t.contains("[]")
        || array_word
        || t.starts_with("array")
        || t.starts_with("struct")
        || t.starts_with("map")
    {
        return (None, false);
    }
    // Temporal names first: they are the ones whose meaning lives past the first word
    // (`timestamp with time zone`), and `interval` would otherwise read as nothing. The classifier is
    // `types::temporal_class`, shared with the declared-DDL reader so the two agree. `time with time
    // zone` is temporal but unmodelled, hence unmappable. An interval with fields (`interval day`)
    // truncates, so like a precision it counts as a qualifier.
    if let Some(class) = crate::types::temporal_class(&t.to_uppercase()) {
        let ty = class.map(Ty::from_prover);
        let qualified = t.contains('(') || (ty == Some(Ty::Interval) && t != "interval");
        return (ty, qualified);
    }
    let qualified = t.contains('(');
    // The classification is `types::map_type`'s, so a name means one thing to both readers. Of its
    // classes only four have a `Ty`: a float, a binary type, and anything it does not name are
    // unmappable, and so is an `UNFAITHFUL` type (`citext`, `char(n)`), which the readers that
    // need it name through `types::unfaithful_type`.
    use crate::types::{scalar_class, Scalar};
    let ty = match scalar_class(&t) {
        Some(Scalar::Int) => Some(Ty::Int),
        Some(Scalar::Numeric) => Some(Ty::Real),
        Some(Scalar::Str) => Some(Ty::Str),
        Some(Scalar::Bool) => Some(Ty::Bool),
        Some(Scalar::Float | Scalar::Binary) | None => None,
    };
    (ty, qualified)
}

// ---------------------------------------------------------------------------
// Name heuristics — the weakest evidence (Conf::Name)
// ---------------------------------------------------------------------------

/// Guess a type from a column's *name*. Deliberately the lowest-confidence source: it encodes the
/// conventions of the corpus these queries came from, not a fact about any schema.
///
/// Hand-rolled matching rather than a regex crate — the patterns are all anchored prefixes,
/// anchored suffixes or plain substrings, and a dependency for that is not worth it.
pub fn name_type(col: &str) -> Option<Ty> {
    let c = col.to_lowercase();
    // Boolean first: `is_deleted` should not be read as `_id`-like by a later rule.
    const BOOL_PRE: &[&str] = &["is_", "has_"];
    const BOOL_SUF: &[&str] = &["deleted", "active", "enabled", "_flag"];
    // `isActive` — camelCase `is` prefix, tested against the original casing.
    let camel_is = col.len() > 2
        && col.starts_with("is")
        && col.as_bytes()[2].is_ascii_uppercase();
    if camel_is
        || BOOL_PRE.iter().any(|p| c.starts_with(p))
        || BOOL_SUF.iter().any(|s| c.ends_with(s))
    {
        return Some(Ty::Bool);
    }
    const INT_SUF: &[&str] = &["_id", "_count", "_num", "_level", "_version", "_seq"];
    if c == "id" || INT_SUF.iter().any(|s| c.ends_with(s)) {
        return Some(Ty::Int);
    }
    // No guess for temporal-sounding names (`created_at`, `_date`). Which temporal type a column
    // has decides which conversions its comparisons get, and a DATE guessed for a TIMESTAMP column
    // would put a conversion where there is none, or leave one out. Other evidence, or none, decides.
    const TS_SUF: &[&str] = &["_at", "_date", "_time", "_timestamp", "_on"];
    if c == "date" || TS_SUF.iter().any(|s| c.ends_with(s)) {
        return None;
    }
    const STR_SUB: &[&str] = &[
        "name", "json", "text", "code", "prefix", "email", "title", "url", "slug", "uuid", "status",
        "message", "label", "token", "path", "description", "desc", "type", "key", "nspname",
        "relname", "attname",
    ];
    if STR_SUB.iter().any(|s| c.contains(s)) {
        return Some(Ty::Str);
    }
    None
}

// ---------------------------------------------------------------------------
// Pass 1: attribution — which base-table column does each identifier name?
// ---------------------------------------------------------------------------

/// A node's identity, the stand-in for Python's `id(node)`.
///
/// The AST is borrowed immutably for the whole of [`infer`] and never moved, so an address
/// is a stable key across the independent sweeps below — which is what lets pass 2 look up what pass
/// 1 concluded about a particular identifier without threading a parallel tree through both.
///
/// `crate::casts` keeps using these keys *after* inference, across a pass that mutates the same
/// tree. That is sound only because of how it mutates; see [`crate::casts`] for the argument.
/// Re-exported under the `internals` feature, which makes this doc "public"; the links
/// below point at private callers on purpose and resolve in the crate's own docs.
#[allow(rustdoc::private_intra_doc_links)]
pub fn nid(e: &Expr) -> usize {
    e as *const Expr as usize
}

/// One SELECT's FROM-clause bindings: what its column references can possibly mean.
#[derive(Default)]
struct ScopeInfo {
    /// alias (or table name, when unaliased) → base table, for `TableFactor::Table` sources only.
    base: HashMap<String, String>,
    /// Aliases of *non*-base sources — derived tables, table functions, aliased nested joins.
    /// Tracked separately so a reference to one resolves to "not a base column" and stops there,
    /// instead of continuing outward and finding an enclosing table that happens to share the alias.
    derived: HashSet<String>,
    /// How many non-base sources this SELECT has, aliased or not.
    nderiv: usize,
}

fn factor_alias(tf: &TableFactor) -> Option<String> {
    let a = match tf {
        TableFactor::Derived { alias, .. }
        | TableFactor::TableFunction { alias, .. }
        | TableFactor::NestedJoin { alias, .. }
        | TableFactor::Function { alias, .. }
        | TableFactor::UNNEST { alias, .. }
        | TableFactor::JsonTable { alias, .. } => alias.as_ref(),
        _ => None,
    };
    a.map(|a| fold_ident(&a.name))
}

/// A relation's name as Postgres identifies it: each part folded by [`fold_ident`], joined by `.`
/// as [`obj_name`] joins them. So `"S".t` is `S.t` and `s.t` is `s.t`, two tables, where a
/// lower-cased [`obj_name`] made them one.
///
/// A part that is not an identifier (a dialect's identifier-generating function) has no spelling
/// whose quoting can be read, so the name is refused rather than folded by a guess.
fn folded_name(n: &ObjectName) -> Result<String> {
    let parts = n
        .0
        .iter()
        .map(|p| p.as_ident().map(fold_ident))
        .collect::<Option<Vec<_>>>()
        .ok_or_else(|| unsupported(format!("table name {n} with a part that is not an identifier")))?;
    Ok(parts.join("."))
}

fn add_factor(tf: &TableFactor, s: &mut ScopeInfo) -> Result<()> {
    match tf {
        TableFactor::Table { name, alias, .. } => {
            // The full dotted name, matching `factor_instance`'s `obj_name(name)` — the catalog this
            // module synthesizes is looked up by `cat.find` with that string, so agreeing with the
            // consumer matters more here than agreeing with sqlglot's schema-stripped `Table.name`.
            // Prepared cases have no schema qualifiers, so the two coincide anyway. Folded, not
            // lower-cased: `cat.find` compares up to case, and `refuse_tables_up_to_case` refuses
            // two names that are one up to case, so the lookup cannot merge `"Orders"` with `orders`.
            let tn = folded_name(name)?;
            let key = alias.as_ref().map(|a| fold_ident(&a.name)).unwrap_or_else(|| tn.clone());
            s.base.insert(key, tn);
        }
        // `(a JOIN b)` without an alias contributes *a*'s and *b*'s columns to this same scope,
        // which is what SQL does; with an alias it is opaque, like a derived table.
        TableFactor::NestedJoin { table_with_joins, alias: None } => {
            add_twj(table_with_joins, s)?;
        }
        other => {
            s.nderiv += 1;
            if let Some(a) = factor_alias(other) {
                s.derived.insert(a);
            }
        }
    }
    Ok(())
}

fn add_twj(twj: &TableWithJoins, s: &mut ScopeInfo) -> Result<()> {
    add_factor(&twj.relation, s)?;
    for j in &twj.joins {
        add_factor(&j.relation, s)?;
    }
    Ok(())
}

fn collect_scope(from: &[TableWithJoins]) -> Result<ScopeInfo> {
    let mut s = ScopeInfo::default();
    for twj in from {
        add_twj(twj, &mut s)?;
    }
    Ok(s)
}

/// Walks the statement structure keeping a stack of enclosing [`ScopeInfo`]s, and attributes every
/// identifier it passes to a base-table column where that is determinable.
///
/// The scope stack is maintained by `pre_visit_select` / `post_visit_select`, which bracket all of a
/// SELECT's children — so when `pre_visit_expr` fires, the top of the stack *is* the identifier's
/// innermost enclosing scope. That is the incremental form of the preprocessor's `innermost_scope`,
/// which walks parent pointers upward instead; sqlparser's AST has no parent pointers, and this
/// needs none. Bracketing on SELECT rather than on `Query` is deliberate: `a UNION b` is one `Query`
/// with two SELECTs in its body, and giving both branches one merged scope would mis-attribute.
struct Attributor<'a> {
    stack: Vec<ScopeInfo>,
    /// A declared schema, when the input has one. Used only to disambiguate a bare column between
    /// several in-scope tables; inference-only inputs pass `None`.
    prov: Option<&'a Catalog>,
    /// identifier node → the base column it names.
    col: HashMap<usize, Atom>,
    /// Every base table any FROM clause mentions, whether or not a column resolved to it.
    all_tables: BTreeSet<String>,
    /// table → the columns the queries actually read from it.
    cols: HashMap<String, BTreeSet<String>>,
    /// `WHERE` / `HAVING` condition roots, for the boolean-context rule in pass 2. Collected here
    /// because a condition is a field of `Select`, not an `Expr` reachable from one.
    bool_ctx: HashSet<usize>,
    /// Identifier nodes that are the field half of a `.`-access, not column references. See
    /// [`Attributor::note_field_access`].
    field_access: HashSet<usize>,
}

/// Whether a declared catalog says `table` has `col`.
///
/// The table is found as lowering finds it ([`Catalog::find`]), and the column by its exact folded
/// name, as lowering resolves it: the catalog stores a column under `dml::fold_ident`'s name, and
/// so does attribution. Compared up to case, `"A"` over `m ("A")` and `t (a)` was declared by both, and
/// an unquoted `A` (which is `t.a`) was declared by `m` as well.
pub fn declares(cat: &Catalog, table: &str, col: &str) -> bool {
    cat.find(table).is_some_and(|i| cat.tables[i].cols.iter().any(|(c, _)| c == col))
}

impl<'a> Attributor<'a> {
    fn new(prov: Option<&'a Catalog>) -> Self {
        Self {
            stack: Vec::new(),
            prov,
            col: HashMap::new(),
            all_tables: BTreeSet::new(),
            cols: HashMap::new(),
            bool_ctx: HashSet::new(),
            field_access: HashSet::new(),
        }
    }

    /// `f(x)."id"` is a field selected out of whatever `f(x)` returns, not a column named `id`. But
    /// sqlparser models it as `CompoundFieldAccess { root, access_chain: [Dot(Identifier("id"))] }`
    /// and `Dot` holds a real `Expr`, so the walk offers that `id` to [`Attributor::attribute`]
    /// exactly as it would a bare column — which then pins it to whatever single table is in scope
    /// and invents a column the schema never had. Mark those identifiers on the way past the parent
    /// (`pre_visit` reaches it first) so `attribute` can skip them.
    fn note_field_access(&mut self, e: &Expr) {
        let Expr::CompoundFieldAccess { access_chain, .. } = e else { return };
        for a in access_chain {
            if let AccessExpr::Dot(f) = a {
                self.field_access.insert(nid(f));
            }
        }
    }

    fn record(&mut self, e: &Expr, table: &str, col: &str) {
        self.col.insert(nid(e), Atom::Col(table.to_string(), col.to_string()));
        self.cols.entry(table.to_string()).or_default().insert(col.to_string());
    }

    /// The nearest enclosing (strictly outer) scope holding a base table that declares `col`, if
    /// exactly one of that scope's tables does. Mirrors `_outer_declaring_base`: a bare column no
    /// in-scope table declares, but an enclosing one does, is a correlated reference outward — and
    /// attributing it to the sole local table instead would give that table a column it never had.
    fn outer_declaring(&self, col: &str, cat: &Catalog) -> Option<String> {
        for s in self.stack.iter().rev().skip(1) {
            let cands: BTreeSet<&String> =
                s.base.values().filter(|t| declares(cat, t, col)).collect();
            if !cands.is_empty() {
                // Ambiguous within that scope -> give up rather than pick.
                return (cands.len() == 1).then(|| (*cands.iter().next().unwrap()).clone());
            }
        }
        None
    }

    fn attribute(&mut self, e: &Expr) -> Result<()> {
        if self.field_access.contains(&nid(e)) {
            return Ok(());
        }
        // Folded as Postgres folds them, which is how lowering resolves the same names: a quoted
        // `"createdAt"` is not `createdat`, and a qualifier `"O"` is not the alias `o`.
        let (qual, col) = match e {
            Expr::Identifier(id) => (None, fold_ident(id)),
            Expr::CompoundIdentifier(parts) if parts.len() >= 2 => (
                Some(fold_ident(&parts[parts.len() - 2])),
                fold_ident(&parts[parts.len() - 1]),
            ),
            _ => return Ok(()),
        };
        if self.stack.is_empty() {
            return Ok(());
        }
        if let Some(q) = qual {
            // Innermost scope that binds the qualifier wins, so a local alias shadows an outer one.
            // (The preprocessor looks only at the innermost scope's *base* tables and then searches
            // enclosing scopes' sources, so a local derived alias there can be shadowed *by* an
            // outer base table of the same name. That is a mis-attribution; this does not copy it.)
            for i in (0..self.stack.len()).rev() {
                if let Some(t) = self.stack[i].base.get(&q).cloned() {
                    self.record(e, &t, &col);
                    return Ok(());
                }
                if self.stack[i].derived.contains(&q) {
                    return Ok(()); // a derived source's column: unresolved, and that is fine
                }
            }
            return Ok(()); // unknown qualifier -> unresolved
        }
        let top = self.stack.last().expect("non-empty stack");
        let (nbase, nderiv) = (top.base.len(), top.nderiv);
        let sole = (nbase == 1).then(|| top.base.values().next().unwrap().clone());
        match self.prov {
            Some(cat) => {
                // Disambiguate against the real schema: the in-scope base table that declares it.
                let cands: Vec<String> =
                    top.base.values().filter(|t| declares(cat, t, &col)).cloned().collect();
                match cands.len() {
                    1 => self.record(e, &cands[0], &col),
                    0 => {
                        if let Some(outer) = self.outer_declaring(&col, cat) {
                            self.record(e, &outer, &col);
                        } else if nderiv == 0 {
                            if let Some(t) = sole {
                                self.record(e, &t, &col);
                            }
                        }
                    }
                    _ => {
                        let by = bindings(top.base.iter().filter(|(_, t)| declares(cat, t, &col)));
                        return Err(schema(format!(
                            "ambiguous unqualified column {col} (declared by {by})"
                        )));
                    }
                }
            }
            None => {
                if let (0, Some(t)) = (nderiv, sole) {
                    self.record(e, &t, &col);
                } else if nbase > 1 {
                    let of = bindings(top.base.iter());
                    return Err(schema(format!(
                        "ambiguous unqualified column {col} (no catalog says which of {of} declares it)"
                    )));
                }
            }
        }
        Ok(())
    }
}

/// In-scope base tables as the FROM clause binds them (`orders o`, or `orders` unaliased), sorted,
/// for a refusal that has to say which tables it could not choose between.
fn bindings<'a>(it: impl Iterator<Item = (&'a String, &'a String)>) -> String {
    let mut v: Vec<String> =
        it.map(|(alias, t)| if alias == t { t.clone() } else { format!("{t} {alias}") }).collect();
    v.sort();
    v.join(", ")
}

impl Visitor for Attributor<'_> {
    type Break = FrontendError;

    /// `ORDER BY` and `LIMIT` hang off `Query`, not off `Select`, and the derived walk reaches them
    /// *after* the body — so by the time `ORDER BY created_at` is visited, `post_visit_select` has
    /// already popped the scope that gives `created_at` a meaning. Re-pushing the body's scope for
    /// the whole `Query` covers them. During the body's own visit the same scope is then on the
    /// stack twice, which changes nothing: bare columns read the top and qualified ones scan down.
    fn pre_visit_query(&mut self, q: &Query) -> ControlFlow<Self::Break> {
        let scope = match q.body.as_ref() {
            sqlparser::ast::SetExpr::Select(s) => collect_scope(&s.from),
            _ => Ok(ScopeInfo::default()),
        };
        match scope {
            Ok(scope) => self.stack.push(scope),
            Err(err) => return ControlFlow::Break(err),
        }
        ControlFlow::Continue(())
    }

    fn post_visit_query(&mut self, _q: &Query) -> ControlFlow<Self::Break> {
        self.stack.pop();
        ControlFlow::Continue(())
    }

    fn pre_visit_select(&mut self, s: &Select) -> ControlFlow<Self::Break> {
        let scope = match collect_scope(&s.from) {
            Ok(scope) => scope,
            Err(err) => return ControlFlow::Break(err),
        };
        self.all_tables.extend(scope.base.values().cloned());
        self.stack.push(scope);
        for c in [s.selection.as_ref(), s.having.as_ref()].into_iter().flatten() {
            self.bool_ctx.insert(nid(c));
        }
        ControlFlow::Continue(())
    }

    fn post_visit_select(&mut self, _s: &Select) -> ControlFlow<Self::Break> {
        self.stack.pop();
        ControlFlow::Continue(())
    }

    fn pre_visit_expr(&mut self, e: &Expr) -> ControlFlow<Self::Break> {
        self.note_field_access(e);
        match self.attribute(e) {
            Ok(()) => ControlFlow::Continue(()),
            Err(err) => ControlFlow::Break(err),
        }
    }
}

// ---------------------------------------------------------------------------
// Pass 2: evidence
// ---------------------------------------------------------------------------

/// Function names sqlglot models with a dedicated AST node rather than `exp.Anonymous`.
///
/// The preprocessor keys an [`Atom::FnResult`] off `exp.Anonymous` only, so a call it *does* have a
/// node for contributes no atom at all. sqlparser has no such split — every call is `Expr::Function`
/// — so reproducing the distinction needs the list restated here. Getting a name wrong changes only
/// how much evidence propagates (an atom that should not exist, or one that should and does not),
/// never whether a type is *asserted*, so the cost is fidelity against the preprocessor rather than
/// soundness. That fidelity was measured rather than argued: on rows with no dropped cast, the two
/// agreed on every column and every parameter, which is what this list buys.
const MODELLED_FNS: &[&str] = &[
    "abs", "array_agg", "avg", "cast", "ceil", "ceiling", "coalesce", "concat", "concat_ws", "count",
    "current_date", "current_time", "current_timestamp", "date", "date_add", "date_diff",
    "date_sub", "date_trunc", "day", "exp", "extract", "floor", "greatest", "group_concat", "if",
    "ifnull", "initcap", "json_extract", "json_extract_scalar", "least", "left", "length", "ln",
    "log", "lower", "lpad", "ltrim", "max", "md5", "min", "mod", "month", "now", "nullif", "nvl",
    "pow", "power", "quantile", "rand", "rank", "regexp_like", "regexp_replace", "repeat",
    "replace", "right", "round", "row_number", "rpad", "rtrim", "sqrt", "stddev", "string_agg",
    "strpos", "substr", "substring", "sum", "time", "timestamp", "trim", "unix_timestamp", "upper",
    "variance", "year",
];

/// The `$N` a placeholder or a substituted `qpN(0)` call refers to.
///
/// Both spellings are accepted because both occur: `$1` in a raw corpus row, and `qp1(0)` after the
/// preprocessor has replaced it with a declared nullary-ish function. Reading the latter is what
/// lets this module be checked against the preprocessor's own output.
pub(crate) fn param_index(e: &Expr) -> Option<u32> {
    match e {
        Expr::Value(v) => match &v.value {
            SqlValue::Placeholder(p) => p.strip_prefix('$')?.parse().ok(),
            _ => None,
        },
        Expr::Function(f) if f.over.is_none() => {
            obj_name(&f.name).to_lowercase().strip_prefix("qp")?.parse().ok()
        }
        _ => None,
    }
}

/// The return type encoded in a name the preprocessor's `normalize_tree` synthesized.
///
/// That stage replaces constructs the prover cannot model with a call to a fresh uninterpreted
/// symbol, and spells the symbol's result type into its name rather than carrying it separately —
/// so `q_str_regexp(x)` returns VARCHAR. `qa_` marks the aggregate forms. `None` for every name a
/// human wrote, whose type has to be inferred like anything else.
pub(crate) fn builtin_rtype(name: &str) -> Option<Ty> {
    let n = name.to_lowercase();
    for (p, t) in [
        ("q_int_", Ty::Int),
        ("q_bool_", Ty::Bool),
        ("q_str_", Ty::Str),
        ("q_op_", Ty::Opaque),
        ("qa_int_", Ty::Int),
        ("qa_bool_", Ty::Bool),
        ("qa_str_", Ty::Str),
        ("qa_op_", Ty::Opaque),
    ] {
        if n.starts_with(p) {
            return Some(t);
        }
    }
    None
}

/// Whether a synthesized symbol is one of the *aggregate* forms, which must be declared
/// `aggregate function` so `lower.rs` routes it to the Group path instead of lowering it per row.
pub(crate) fn is_agg_name(name: &str) -> bool {
    name.to_lowercase().starts_with("qa_")
}

/// A function call whose result type nobody declared — the analogue of `exp.Anonymous`.
pub(crate) fn is_opaque_call(e: &Expr) -> bool {
    match e {
        Expr::Function(f) => {
            let n = obj_name(&f.name).to_lowercase();
            !MODELLED_FNS.contains(&n.as_str()) && !n.starts_with("qp")
        }
        _ => false,
    }
}

/// The atom a node carries a type *for*, if any. `None` for anything whose type is not a fact about
/// one nameable thing — a derived column, an arithmetic subexpression, a literal.
fn side_key(at: &Attributor, e: &Expr) -> Option<Atom> {
    match e {
        Expr::Identifier(_) | Expr::CompoundIdentifier(_) => at.col.get(&nid(e)).cloned(),
        _ => {
            if let Some(n) = param_index(e) {
                Some(Atom::Param(n))
            } else if is_opaque_call(e) {
                Some(Atom::FnResult(nid(e)))
            } else {
                None
            }
        }
    }
}

/// The type a node contributes *by itself*, with its confidence.
fn side_type(e: &Expr) -> Result<Option<(Ty, Conf)>> {
    match e {
        // An unmappable target contributes nothing rather than refusing: the cast rewrite models it
        // as an uninterpreted `qcast`, and this pass has no type to offer for the result of one.
        Expr::Cast { data_type, .. } => {
            Ok(map_type_name(&data_type.to_string()).0.map(|t| (t, Conf::Cast)))
        }
        Expr::Value(v) => Ok(literal_type(&v.value).map(|t| (t, Conf::Use))),
        _ => Ok(None),
    }
}

/// A literal's type. `NULL` contributes nothing — it inhabits every type.
pub(crate) fn literal_type(v: &SqlValue) -> Option<Ty> {
    match v {
        SqlValue::Number(txt, _) => {
            Some(if txt.contains('.') || txt.to_lowercase().contains('e') { Ty::Real } else { Ty::Int })
        }
        SqlValue::SingleQuotedString(_)
        | SqlValue::DoubleQuotedString(_)
        | SqlValue::EscapedStringLiteral(_)
        | SqlValue::NationalStringLiteral(_)
        | SqlValue::HexStringLiteral(_)
        | SqlValue::UnicodeStringLiteral(_)
        | SqlValue::SingleQuotedByteStringLiteral(_)
        | SqlValue::DoubleQuotedByteStringLiteral(_)
        | SqlValue::SingleQuotedRawStringLiteral(_)
        | SqlValue::DoubleQuotedRawStringLiteral(_) => Some(Ty::Str),
        SqlValue::Boolean(_) => Some(Ty::Bool),
        _ => None,
    }
}

/// Run `f` over every expression of `q` in pre-order, turning a refusal into `Err`.
///
/// One sweep per rule, in the preprocessor's order, rather than one sweep with a big `match`.
/// [`Uf::merge`] discards the weaker of two disagreeing facts, so *when* a fact arrives decides
/// which survives whenever two of equal confidence disagree — sweep order is part of the behaviour
/// being ported, not an implementation detail to optimise away.
pub(crate) fn sweep<F>(q: &Query, mut f: F) -> Result<()>
where
    F: FnMut(&Expr) -> Result<()>,
{
    let r = visit_expressions(q, |e| match f(e) {
        Ok(()) => ControlFlow::Continue(()),
        Err(err) => ControlFlow::Break(err),
    });
    match r {
        ControlFlow::Continue(()) => Ok(()),
        ControlFlow::Break(err) => Err(err),
    }
}

/// A function call's positional arguments, ignoring `*` and named forms.
fn call_args(e: &Expr) -> &[FunctionArg] {
    match e {
        Expr::Function(f) => match &f.args {
            FunctionArguments::List(l) => &l.args,
            _ => &[],
        },
        _ => &[],
    }
}

fn first_arg(e: &Expr) -> Option<&Expr> {
    match call_args(e).first() {
        Some(FunctionArg::Unnamed(FunctionArgExpr::Expr(x))) => Some(x),
        _ => None,
    }
}

/// Collect typed evidence from one query into `uf`. The port of `gather_types`.
fn gather_types(at: &Attributor, q: &Query, uf: &mut Uf) -> Result<()> {
    // 1. Name-based seed (weakest).
    sweep(q, |e| {
        if let (Some(k), Some(t)) = (side_key(at, e), column_name(e).and_then(|c| name_type(&c))) {
            uf.set_type(&k, t, Conf::Name)?;
        }
        Ok(())
    })?;

    // 2. `$N::T` states a parameter's type outright. An unmappable `T` states nothing and is passed
    //    over: the parameter is then typed by whatever other evidence reaches it, or read as
    //    `Ty::Opaque` further down, which is where that default already lives.
    sweep(q, |e| {
        if let Expr::Cast { expr, data_type, .. } = e {
            if let Some(t) = map_type_name(&data_type.to_string()).0 {
                if let Some(n) = param_index(expr) {
                    uf.set_type(&Atom::Param(n), t, Conf::Cast)?;
                }
            }
        }
        Ok(())
    })?;

    // 3. Comparisons: the two sides share a type, and each side's own type informs the other.
    sweep(q, |e| {
        let Expr::BinaryOp { left, op, right } = e else { return Ok(()) };
        if !matches!(
            op,
            BinaryOperator::Eq
                | BinaryOperator::NotEq
                | BinaryOperator::Gt
                | BinaryOperator::Lt
                | BinaryOperator::GtEq
                | BinaryOperator::LtEq
        ) {
            return Ok(());
        }
        let (kl, kr) = (side_key(at, left), side_key(at, right));
        let (tl, tr) = (side_type(left)?, side_type(right)?);
        if let (Some(a), Some(b)) = (&kl, &kr) {
            uf.union(a, b)?;
        }
        if let (Some(a), Some((t, c))) = (&kl, tr) {
            uf.set_type(a, t, c)?;
        }
        if let (Some(b), Some((t, c))) = (&kr, tl) {
            uf.set_type(b, t, c)?;
        }
        Ok(())
    })?;

    // 4. Arithmetic operands are numeric. Only a name-strength guess: `a - b` over two timestamps is
    //    an interval, and the corpus has those.
    sweep(q, |e| {
        let Expr::BinaryOp { left, op, right } = e else { return Ok(()) };
        if !matches!(
            op,
            BinaryOperator::Plus
                | BinaryOperator::Minus
                | BinaryOperator::Multiply
                | BinaryOperator::Divide
                | BinaryOperator::Modulo
        ) {
            return Ok(());
        }
        let real = [left, right].iter().any(|o| matches!(side_type(o), Ok(Some((Ty::Real, _)))));
        for o in [left, right] {
            if let Some(k) = side_key(at, o) {
                uf.set_type(&k, if real { Ty::Real } else { Ty::Int }, Conf::Name)?;
            }
        }
        Ok(())
    })?;

    // 5. `x LIKE p` makes x a string.
    sweep(q, |e| {
        let inner = match e {
            Expr::Like { expr, .. } | Expr::ILike { expr, .. } => expr,
            _ => return Ok(()),
        };
        if let Some(k) = side_key(at, inner) {
            uf.set_type(&k, Ty::Str, Conf::Use)?;
        }
        Ok(())
    })?;

    // 6. Boolean context: an operand of NOT/AND/OR, a WHERE or HAVING condition, or `x IS TRUE`.
    //    Without this the column defaults to OPAQUE and `NOT <VARBINARY>` is rejected downstream --
    //    a refusal caused by the absence of evidence rather than by the query.
    sweep(q, |e| {
        let mut ops: Vec<&Expr> = Vec::new();
        match e {
            Expr::UnaryOp { op: sqlparser::ast::UnaryOperator::Not, expr } => ops.push(expr),
            Expr::BinaryOp { left, op: BinaryOperator::And | BinaryOperator::Or, right } => {
                ops.push(left);
                ops.push(right);
            }
            Expr::IsTrue(x) | Expr::IsNotTrue(x) | Expr::IsFalse(x) | Expr::IsNotFalse(x) => {
                ops.push(x)
            }
            _ => {}
        }
        if at.bool_ctx.contains(&nid(e)) {
            ops.push(e);
        }
        for o in ops {
            if let Some(k) = side_key(at, o) {
                uf.set_type(&k, Ty::Bool, Conf::Use)?;
            }
        }
        Ok(())
    })?;

    // 7. SUM/AVG take a number.
    sweep(q, |e| {
        let Expr::Function(f) = e else { return Ok(()) };
        let n = obj_name(&f.name).to_lowercase();
        if n != "sum" && n != "avg" {
            return Ok(());
        }
        if let Some(k) = first_arg(e).and_then(|a| side_key(at, a)) {
            uf.set_type(&k, Ty::Int, Conf::Name)?;
        }
        Ok(())
    })?;

    // 7b. A `LIMIT`/`OFFSET` count is an integer — the grammar says so, which is why this is
    //     `Conf::Cast`-strength rather than a guess like pass 7's.
    //
    //     This is not cosmetic. The prover evaluates the count into a z3 term and congruence on the
    //     `limit` HOp asserts the two sides' counts equal, so a count that lands on any other sort
    //     panics z3 with `SortDiffers`. Measured: without this pass `LIMIT $1` against `LIMIT 1`
    //     crashed (VARBINARY against Int, from `$1` having no other evidence), and so did
    //     `LIMIT $5` against `LIMIT $4` where `$4` had been unified into a boolean position.
    //
    //     Walks queries rather than expressions because the count hangs off `Query`, and `sweep`
    //     hands out expressions with no idea of the position they sit in.
    let counts = pagination_count_ids(q);
    sweep(q, |e| {
        if counts.contains(&nid(e)) {
            if let Some(k) = side_key(at, e) {
                uf.set_type(&k, Ty::Int, Conf::Cast)?;
            }
        }
        Ok(())
    })?;

    // 8. CASE branches share a type (SQL requires it), so unify them and let any concrete branch
    //    type the rest. `CASE WHEN p THEN f(x) ELSE col END` type-checks only because of this.
    sweep(q, |e| {
        let Expr::Case { conditions, else_result, .. } = e else { return Ok(()) };
        let mut parts: Vec<&Expr> = conditions.iter().map(|w| &w.result).collect();
        if let Some(d) = else_result {
            parts.push(d);
        }
        let keys: Vec<Atom> = parts.iter().filter_map(|p| side_key(at, p)).collect();
        for k in keys.iter().skip(1) {
            uf.union(&keys[0], k)?;
        }
        if let Some(first) = keys.first() {
            for p in &parts {
                if let Some((t, c)) = side_type(p)? {
                    uf.set_type(first, t, c)?;
                }
            }
        }
        Ok(())
    })
}

/// The [`nid`]s of every `LIMIT`/`OFFSET`/`FETCH` count expression in `q`, at any nesting depth.
///
/// Node *ids* rather than references because the derived `Visit` walk hands `pre_visit_query` a
/// borrow with no lifetime relation to the visitor, so references cannot escape it — and because
/// [`sweep`] is then the natural way to get from an id back to the expression, which is what
/// [`side_key`] needs.
fn pagination_count_ids(q: &Query) -> HashSet<usize> {
    use sqlparser::ast::LimitClause;

    struct Counts(HashSet<usize>);
    impl Visitor for Counts {
        type Break = std::convert::Infallible;

        fn pre_visit_query(&mut self, q: &Query) -> ControlFlow<Self::Break> {
            match &q.limit_clause {
                Some(LimitClause::LimitOffset { limit, offset, .. }) => {
                    self.0.extend(limit.as_ref().map(nid));
                    self.0.extend(offset.as_ref().map(|o| nid(&o.value)));
                }
                Some(LimitClause::OffsetCommaLimit { offset, limit }) => {
                    self.0.insert(nid(offset));
                    self.0.insert(nid(limit));
                }
                None => {}
            }
            if let Some(f) = &q.fetch {
                self.0.extend(f.quantity.as_ref().map(nid));
            }
            ControlFlow::Continue(())
        }
    }

    let mut v = Counts(HashSet::new());
    let ControlFlow::Continue(()) = q.visit(&mut v);
    v.0
}

/// The bare column name an identifier expression ends in.
fn column_name(e: &Expr) -> Option<String> {
    match e {
        Expr::Identifier(id) => Some(id.value.clone()),
        Expr::CompoundIdentifier(p) => p.last().map(|i| i.value.clone()),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------

/// What inference concluded about a pair.
pub struct Inferred {
    /// The synthesized base-table schema, built only when [`infer`] was given no declared catalog:
    /// `None` exactly when it was given one, since a pair with a declared catalog is lowered against
    /// that catalog, and refusing for a catalog nothing reads would refuse pairs lowering can lower.
    pub catalog: Option<Catalog>,
    /// `$N` → its type, for every parameter either query mentions. No evidence means
    /// [`Ty::Opaque`], the same fail-safe the columns get.
    pub params: std::collections::BTreeMap<u32, Ty>,
    /// Identifier node ([`nid`]) → the base column it names.
    ///
    /// Kept rather than discarded because the cast rewrite has to ask the same question inference
    /// already answered — "what is this operand's type?" — and re-deriving it from the catalog would
    /// silently differ wherever attribution failed but a *use* still supplied evidence.
    pub col: HashMap<usize, Atom>,
    /// Every unmodelled call's result type, keyed by [`nid`]: the port of the preprocessor's
    /// `fn_ret`. A name that encodes its own type wins; otherwise whatever the evidence settled on;
    /// otherwise [`Ty::Opaque`].
    pub fn_ret: HashMap<usize, Ty>,
    /// The solved evidence, for operand types the maps above do not cover.
    pub uf: Uf,
}

/// Infer a [`Catalog`] for `queries`, which are assumed to be the two sides of one pair.
///
/// The catalog alone, for tests that assert on the synthesized schema. The shipping path calls
/// [`infer`] and keeps the rest of the [`Inferred`]. No declared catalog, since only then is one
/// synthesized.
#[cfg(test)]
fn infer_catalog(queries: &[Query]) -> Result<Catalog> {
    Ok(infer(queries, None)?.catalog.expect("synthesized when no catalog is declared"))
}

/// Infer a schema and parameter types for `queries`, the two sides of one pair.
///
/// One [`Uf`] is shared across both sides on purpose: a type the left query states outright must
/// reach the right query's copy of the same column, or the pair gets two different schemas and the
/// comparison is meaningless. `prov` supplies a declared schema when one exists, used only to
/// disambiguate bare columns and to seed authoritative types. With one, no catalog is synthesized
/// ([`Inferred::catalog`] is `None`): the pair is lowered against the declared catalog, which
/// refuses what it cannot lower for reasons of its own. Without one, [`build_inferred`] synthesizes
/// it, and refuses a pair it cannot synthesize a table for.
pub fn infer(queries: &[Query], prov: Option<&Catalog>) -> Result<Inferred> {
    let mut at = Attributor::new(prov);
    for q in queries {
        if q.with.is_some() {
            // Nothing here tracks CTE names, so a reference to one would be attributed to whatever
            // base table shares its alias. `lower.rs` has no `WITH` handling either, so refusing is
            // the honest answer rather than a gap being papered over.
            return Err(unsupported("WITH (CTE) during type inference"));
        }
        if let ControlFlow::Break(err) = q.visit(&mut at) {
            return Err(err);
        }
    }
    let mut uf = Uf::new();
    // Declared types are authoritative and are seeded first, so they win over every heuristic below
    // and propagate through comparisons to the parameters.
    if let Some(cat) = prov {
        uf.on_query(Origin::DECLARED);
        for (t, cols) in &at.cols {
            if let Some(i) = cat.find(t) {
                for c in cols {
                    // `c` is folded as Postgres folds it, as every attributed name is, and so is the
                    // catalog's name for a column: an exact match is the column lowering reads.
                    if let Some((_, ty)) = cat.tables[i].cols.iter().find(|(n, _)| n == c) {
                        // Seeded even when the declared type is one nothing recognises: `Opaque` at
                        // `Schema` confidence is a *fact* -- the DDL says this column holds
                        // something we do not model -- and it has to outrank a name guess, or a
                        // column called `status` declared as an enum comes back VARCHAR.
                        let k = Ty::from_prover(ty);
                        uf.set_type(&Atom::Col(t.clone(), c.clone()), k, Conf::Schema)?;
                    }
                }
            }
        }
    }
    // One query at a time, so every piece of evidence `gather_types` records is attributed to the
    // query that argued for it. This is the only place that knows which query is being walked.
    for (i, q) in queries.iter().enumerate() {
        uf.on_query(Origin::query(i));
        gather_types(&at, q, &mut uf)?;
    }
    // Every parameter either query mentions, in either spelling.
    let mut seen: BTreeSet<u32> = BTreeSet::new();
    for q in queries {
        sweep(q, |e| {
            if let Some(n) = param_index(e) {
                seen.insert(n);
            }
            Ok(())
        })?;
    }
    let params = seen
        .into_iter()
        .map(|n| (n, uf.get_type(&Atom::Param(n)).unwrap_or(Ty::Opaque)))
        .collect();
    // Every unmodelled call's result type, resolved once here while the tree is still the one the
    // atoms were keyed against. The preprocessor builds the same map at the same point.
    let mut fn_ret: HashMap<usize, Ty> = HashMap::new();
    for q in queries {
        sweep(q, |e| {
            if is_opaque_call(e) {
                let name = match e {
                    Expr::Function(f) => obj_name(&f.name),
                    _ => unreachable!("is_opaque_call matched a non-call"),
                };
                let t = builtin_rtype(&name)
                    .or_else(|| uf.get_type(&Atom::FnResult(nid(e))))
                    .unwrap_or(Ty::Opaque);
                fn_ret.insert(nid(e), t);
            }
            Ok(())
        })?;
    }
    let catalog = match prov {
        // Attribution reads each table by its folded name under both catalogs, so the check that
        // no two of the pair's tables are one name up to case holds for both; `build_inferred`
        // makes it first.
        Some(_) => {
            refuse_tables_up_to_case(&at.all_tables, &at.cols)?;
            None
        }
        None => Some(build_inferred(&at.all_tables, &at.cols, &mut uf)?),
    };
    Ok(Inferred { catalog, params, col: at.col, fn_ret, uf })
}

/// The pair with the second query's parameters renumbered clear of the first's — the counterfactual
/// input. `None` for anything that is not a pair.
///
/// Renumbering by `max index + 1` makes the two `$N` sets disjoint, and that is *precisely* dropping
/// the identification of the two queries' parameters while changing nothing else. Everything the pair
/// shares otherwise is still shared — in particular the one [`Uf`] spanning both queries for every
/// *column*, so a name guess in one still meets the other's declared type.
///
/// Written once because two gates decide by it: [`infers_with_split_params`] for inference, and the
/// crate's `lowers_with_split_params` for lowering. Both are read by [`crate::params::root_cause`] and
/// [`crate::params::root_cause_lowered`], and they have to be asking the same question for the rule
/// those two share to mean one thing.
pub fn split_params(queries: &[Query]) -> Option<Vec<Query>> {
    if queries.len() != 2 {
        return None;
    }
    let mut split = queries.to_vec();
    let top = [&queries[0], &queries[1]].iter().filter_map(|q| max_param(q)).max().unwrap_or(0);
    shift_params(&mut split[1], top + 1);
    Some(split)
}

/// Would the pair have typed if the two queries' parameters were *not* the same symbols?
///
/// The counterfactual behind [`crate::params::root_cause`], and the sharp form of the question the
/// type evidence cannot answer (see [`Origin`]). The pair is [`split_params`]-renumbered and inferred
/// again; if inference then succeeds, identifying the two queries' `$N` is the whole reason it failed.
///
/// Not a general "is this conflict cross-query" test, and deliberately narrower than one: a
/// disagreement between the two queries' column evidence survives the renumbering and answers `false`,
/// which is what makes this usable as evidence *about parameters*.
///
/// Called only after inference has already failed, where one more pass costs nothing and the pair is
/// refused either way — nothing about a verdict rides on the answer, only which reason the row
/// reports. `false` for anything that is not a pair, so a caller cannot read a promotion out of an
/// input this cannot reason about.
pub fn infers_with_split_params(queries: &[Query], prov: Option<&Catalog>) -> bool {
    match split_params(queries) {
        Some(split) => infer(&split, prov).is_ok(),
        None => false,
    }
}

/// The largest parameter index `q` mentions, in either spelling.
fn max_param(q: &Query) -> Option<u32> {
    let mut top = None;
    // The closure never refuses.
    let _ = sweep(q, |e| {
        top = top.max(param_index(e));
        Ok(())
    });
    top
}

/// Renumber every parameter in `q` upward by `by`, keeping each in the spelling it was written in.
///
/// Bottom-up (`post_visit_expr`), and a parameter is a leaf, so no renumbered node is renumbered
/// twice. The `qpN` spelling is renamed rather than rewritten as `$N`: this walks a *copy* whose only
/// consumer is [`infer`], and giving that copy a form the rest of the crate never produces would be a
/// trap for the next reader.
fn shift_params(q: &mut Query, by: u32) {
    let _ = visit_expressions_mut(q, |e| {
        if let Some(n) = param_index(e) {
            match e {
                Expr::Value(v) => v.value = SqlValue::Placeholder(format!("${}", n + by)),
                Expr::Function(f) => {
                    f.name = ObjectName::from(vec![Ident::new(format!("qp{}", n + by))])
                }
                _ => unreachable!("param_index matched neither spelling"),
            }
        }
        ControlFlow::<()>::Continue(())
    });
}

/// Refuse a pair that names two tables whose names are one up to case: `"Orders"` and `orders`, or
/// `"S".t` and `s.t`.
///
/// Kept apart they are two tables in Postgres, but lowering finds a table by [`Catalog::find`], up
/// to case, so found in one slot both sides read one. [`build_inferred`], which stores a
/// synthesized table under its lower-cased name, makes this check first, as
/// [`Catalog::check_case_collisions`] refuses a DDL that declares two such tables.
///
/// [`infer`] makes it under the seeded catalog too, where nothing is synthesized. That is the
/// conservative choice rather than a necessary one: with only `orders` declared, the declared
/// catalog lowers a pair that reads `"Orders"` against `orders` by finding both in `orders`, and the
/// seeded catalog refuses it.
fn refuse_tables_up_to_case(
    all_tables: &BTreeSet<String>,
    cols: &HashMap<String, BTreeSet<String>>,
) -> Result<()> {
    let mut stored: HashMap<String, &String> = HashMap::new();
    for t in all_tables.iter().chain(cols.keys()).collect::<BTreeSet<_>>() {
        if let Some(other) = stored.insert(t.to_lowercase(), t) {
            return Err(unsupported(format!(
                "two tables named {} up to case: {other} and {t}",
                t.to_lowercase()
            )));
        }
    }
    Ok(())
}

/// Why [`build_inferred`] has no column to synthesize for the tables in `unread`: no column
/// reference was attributed to them. And, where the pair names another table with the same last
/// name, that the two are two tables, since a bare `t` and a qualified `s.t` look like one.
fn unread_reason(unread: &[&String], all_tables: &BTreeSet<String>) -> String {
    let them = if unread.len() == 1 { "it" } else { "any of them" };
    let mut reason = format!(
        "no column reference is attributed to {them}, as when a table is read only through *, \
         count(*), a constant or a USING list, so its columns cannot be inferred"
    );
    let mut by_last: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    for t in all_tables {
        by_last.entry(t.rsplit('.').next().unwrap_or(t)).or_default().push(t);
    }
    for same in by_last.values().filter(|same| same.len() > 1) {
        if same.iter().any(|t| unread.iter().any(|u| u.as_str() == *t)) {
            let (last, rest) = same.split_last().expect("more than one");
            let n = if same.len() == 2 { "two" } else { "different" };
            reason.push_str(&format!("; {} and {last} are {n} tables", rest.join(", ")));
        }
    }
    reason
}

/// Assemble a [`Catalog`] from inferred column types: the catalog `--infer` lowers against, when
/// the pair has no declared one. Under the seeded catalog nothing is synthesized (see [`infer`]).
///
/// `cols` lists, per table, the columns the queries actually read — inference only knows about
/// those, so the synthesized table has exactly them, sorted, matching the preprocessor's rendering
/// so the two can be compared column by column. `all_tables` is every base table the FROM clauses
/// mention: one with no attributed column at all cannot be synthesized, and pretending it has no
/// columns would silently change what `SELECT *` means. So a pair is refused when a table it reads
/// has none, as when the table is read only through `*`, `count(*)`, a constant or a `USING` list:
/// as `table without referenced columns`, or, when no table has one, as a schema error. `no base
/// tables` is reserved for a pair that names no table.
///
/// Everything is nullable and there are no keys: both are constraints that *shrink* the space of
/// instances the prover quantifies over, so inventing either could turn a non-equivalence into a
/// proof. Only a declared DDL may supply them.
///
/// The tables and columns arrive named as Postgres identifies them (see [`Atom::Col`]). A column
/// keeps that name, which is the one lowering resolves. A table's name is lower-cased, as the
/// declared catalog stores it, because lowering finds a table by [`Catalog::find`], up to case. That
/// is sound only where no two of the pair's tables are one name up to case, so a pair that names
/// two is refused first ([`refuse_tables_up_to_case`]).
pub fn build_inferred(
    all_tables: &BTreeSet<String>,
    cols: &HashMap<String, BTreeSet<String>>,
    uf: &mut Uf,
) -> Result<Catalog> {
    refuse_tables_up_to_case(all_tables, cols)?;
    let mut names: Vec<&String> = cols.keys().filter(|t| !cols[*t].is_empty()).collect();
    // By the name each table is stored under, unique once the check above has passed.
    names.sort_by_key(|t| t.to_lowercase());
    let unread: Vec<&String> =
        all_tables.iter().filter(|t| cols.get(*t).is_none_or(BTreeSet::is_empty)).collect();
    if names.is_empty() {
        if unread.is_empty() {
            return Err(schema("no base tables"));
        }
        let list: Vec<&str> = unread.iter().map(|t| t.as_str()).collect();
        return Err(schema(format!(
            "no base table with a referenced column: {} ({})",
            list.join(", "),
            unread_reason(&unread, all_tables)
        )));
    }
    if let Some(missing) = unread.first() {
        return Err(unsupported(format!(
            "table without referenced columns: {missing} ({})",
            unread_reason(&[missing], all_tables)
        )));
    }
    let mut tables = Vec::new();
    for t in names {
        let cols: Vec<(String, String)> = cols[t]
            .iter()
            .map(|c| {
                let ty = uf.get_type(&Atom::Col(t.clone(), c.clone())).unwrap_or(Ty::Opaque);
                (c.clone(), ty.sql().to_string())
            })
            .collect();
        let n = cols.len();
        tables.push(Table {
            name: t.to_lowercase(),
            n_declared: n,
            cols,
            // No DDL was read, so no type was spelled. No DML reduction runs on this catalog.
            declared_types: vec![String::new(); n],
            nullable: vec![true; n],
            // No DDL was read, so no column is known to be of a type whose `=` is identity.
            opaque_identity: vec![false; n],
            // No DDL was read, so no column's default is known. The synthesized table holds only
            // the columns the queries name, so an `INSERT` here cannot omit one anyway.
            row_determined: vec![false; n],
            keys: Vec::new(),
            // No DDL was read, so no column declares a collation: the synthesized schema is one
            // in which every string has the database's default.
            collations: vec![crate::collation::Collation::Default; n],
        });
    }
    Ok(Catalog { tables })
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqlparser::ast::Statement;
    use sqlparser::parser::Parser;

    fn parse(sql: &str) -> Vec<Query> {
        Parser::parse_sql(&crate::DIALECT, sql)
            .expect("parses")
            .into_iter()
            .map(|s| match s {
                Statement::Query(q) => *q,
                other => panic!("not a query: {other}"),
            })
            .collect()
    }

    /// `(table, column, prover type)` for every synthesized column, sorted.
    fn schema_of(sql: &str) -> Vec<(String, String, String)> {
        let cat = infer_catalog(&parse(sql)).expect("inferable");
        cat.tables
            .iter()
            .flat_map(|t| t.cols.iter().map(|(c, ty)| (t.name.clone(), c.clone(), ty.clone())))
            .collect()
    }

    fn err_of(sql: &str) -> String {
        match infer_catalog(&parse(sql)) {
            Err(e) => e.to_string(),
            Ok(_) => panic!("should refuse: {sql}"),
        }
    }

    #[test]
    fn confidence_beats_disagreement_and_ties_refuse() {
        let mut uf = Uf::new();
        let a = Atom::Col("t".into(), "x".into());
        // A name guess loses to a cast.
        uf.set_type(&a, Ty::Int, Conf::Name).unwrap();
        uf.set_type(&a, Ty::Str, Conf::Cast).unwrap();
        assert_eq!(uf.get_type(&a), Some(Ty::Str));
        // Two disagreeing *equal*-confidence facts refuse rather than pick.
        let b = Atom::Col("t".into(), "y".into());
        uf.set_type(&b, Ty::Int, Conf::Use).unwrap();
        let err = uf.set_type(&b, Ty::Str, Conf::Use).unwrap_err();
        assert!(err.to_string().contains("type conflict"), "{err}");
    }

    #[test]
    fn union_propagates_a_type_across_a_comparison() {
        let mut uf = Uf::new();
        let (x, y) = (Atom::Col("a".into(), "x".into()), Atom::Col("b".into(), "y".into()));
        uf.union(&x, &y).unwrap();
        uf.set_type(&y, Ty::Real, Conf::Cast).unwrap();
        assert_eq!(uf.get_type(&x), Some(Ty::Real));
    }

    #[test]
    fn union_of_conflicting_classes_refuses() {
        let mut uf = Uf::new();
        let (x, y) = (Atom::Col("a".into(), "x".into()), Atom::Col("b".into(), "y".into()));
        uf.set_type(&x, Ty::Int, Conf::Use).unwrap();
        uf.set_type(&y, Ty::Str, Conf::Use).unwrap();
        assert!(uf.union(&x, &y).is_err());
    }

    #[test]
    fn a_conflict_names_the_atom_and_the_queries_that_disagreed() {
        let mut uf = Uf::new();
        let p = Atom::Param(2);
        uf.on_query(Origin::query(0));
        uf.set_type(&p, Ty::Bool, Conf::Use).unwrap();
        uf.on_query(Origin::query(1));
        let m = uf.set_type(&p, Ty::Int, Conf::Use).unwrap_err().to_string();
        // Without the atom, `type conflict BOOLEAN/INTEGER` cannot be told from a conflict on a
        // column, which is the distinction that says whether a renumbering is the cause.
        assert!(m.contains("type conflict BOOLEAN/INTEGER at $2"), "{m}");
        assert!(m.contains("(query A vs query B)"), "{m}");
    }

    #[test]
    fn a_conflict_inside_one_query_says_so() {
        let mut uf = Uf::new();
        let c = Atom::Col("t".into(), "x".into());
        uf.on_query(Origin::query(0));
        uf.set_type(&c, Ty::Str, Conf::Use).unwrap();
        let m = uf.set_type(&c, Ty::Int, Conf::Use).unwrap_err().to_string();
        assert!(m.contains("at t.x (query A)"), "{m}");
    }

    #[test]
    fn union_names_both_atoms_it_was_joining() {
        let mut uf = Uf::new();
        let (p, c) = (Atom::Param(1), Atom::Col("t".into(), "x".into()));
        uf.on_query(Origin::query(0));
        uf.set_type(&p, Ty::Str, Conf::Use).unwrap();
        uf.on_query(Origin::query(1));
        uf.set_type(&c, Ty::Int, Conf::Use).unwrap();
        let m = uf.union(&p, &c).unwrap_err().to_string();
        assert!(m.contains("unifying $1 with t.x"), "{m}");
        assert!(m.contains("(query A vs query B)"), "{m}");
    }

    #[test]
    fn agreeing_evidence_unions_its_provenance() {
        // Why disjoint origins are not a cross-query test: two redundant name guesses that agree
        // with the DDL leave the class owned by both queries, so a later conflict with it reports an
        // overlapping origin no matter which query's union brought the classes together. A real pair
        // has this shape, and it is exactly the row a cross-query reading has to get right.
        let mut uf = Uf::new();
        let c = Atom::Col("dogs".into(), "id".into());
        uf.on_query(Origin::DECLARED);
        uf.set_type(&c, Ty::Int, Conf::Schema).unwrap();
        for s in [Origin::query(0), Origin::query(1)] {
            uf.on_query(s);
            uf.set_type(&c, Ty::Int, Conf::Name).unwrap();
        }
        uf.on_query(Origin::query(1));
        let p = Atom::Param(2);
        uf.set_type(&p, Ty::Bool, Conf::Schema).unwrap();
        let m = uf.union(&c, &p).unwrap_err().to_string();
        assert!(m.contains("unifying dogs.id with $2"), "{m}");
        assert!(m.contains("(queries A,B vs query B)"), "{m}");
    }

    /// The counterfactual `params::root_cause` runs on: a conflict that exists only because the two
    /// queries' `$1` are one symbol. Reduced from a real pair, where query A's `$2` is `org_id` and
    /// query B's is `name`.
    #[test]
    fn a_conflict_the_shared_parameters_create_disappears_when_they_are_split() {
        let qs = parse("SELECT x FROM t WHERE t.id = $1; SELECT x FROM t WHERE t.name = $1;");
        assert!(infer(&qs, None).is_err(), "the pair should conflict on $1");
        assert!(infers_with_split_params(&qs, None));
    }

    /// The other half of the rule: a disagreement inside one query is not the parameters' fault, and
    /// renumbering them does not make it go away. A real pair has this shape — a join predicate
    /// unifying two columns whose declared types differ — and it keeps reporting `type conflict`.
    #[test]
    fn a_conflict_inside_one_query_survives_the_split() {
        let qs = parse("SELECT x FROM t WHERE t.id = t.name; SELECT x FROM t WHERE t.id = $1;");
        assert!(infer(&qs, None).is_err());
        assert!(!infers_with_split_params(&qs, None));
    }

    /// The renumbering has to clear *every* index the pair uses, not shift by one: landing query B's
    /// `$1` on query A's `$2` would identify a different wrong pair of atoms and answer `false` for a
    /// reason that has nothing to do with the pair.
    #[test]
    fn the_renumbering_cannot_itself_create_a_conflict() {
        let qs =
            parse("SELECT x FROM t WHERE t.id = $1 AND t.name = $2; SELECT x FROM t WHERE t.id = $1;");
        assert!(infer(&qs, None).is_ok(), "nothing wrong with this pair");
        assert!(infers_with_split_params(&qs, None));
    }

    #[test]
    fn parameters_and_function_results_are_distinct_atoms() {
        let mut uf = Uf::new();
        uf.set_type(&Atom::Param(1), Ty::Int, Conf::Cast).unwrap();
        assert_eq!(uf.get_type(&Atom::Param(2)), None);
        uf.set_type(&Atom::FnResult(0), Ty::Str, Conf::Use).unwrap();
        assert_eq!(uf.get_type(&Atom::FnResult(1)), None);
    }

    #[test]
    fn unmappable_targets_get_no_type_of_their_own() {
        assert_eq!(map_type_name("jsonb").0, None);
        assert_eq!(map_type_name("integer[]").0, None);
        assert_eq!(map_type_name("ARRAY<UUID>").0, None);
        // The SQL-standard array spellings, which would otherwise map by their leading word.
        assert_eq!(map_type_name("INT ARRAY").0, None);
        assert_eq!(map_type_name("integer ARRAY[4]").0, None);
        assert_eq!(map_type_name("double precision").0, None);
        assert_eq!(map_type_name("geometry").0, None);
        // Mapped, with the length qualifier flagged so an identity cast is not dropped.
        assert_eq!(map_type_name("varchar(8)"), (Some(Ty::Str), true));
        assert_eq!(map_type_name("varchar"), (Some(Ty::Str), false));
        // The temporal names: each its own type, whatever the spelling, and an interval with fields
        // counted as qualified (it truncates). `time with time zone` is unmodelled, so unmappable.
        assert_eq!(map_type_name("timestamptz"), (Some(Ty::TimestampTz), false));
        assert_eq!(map_type_name("timestamp with time zone"), (Some(Ty::TimestampTz), false));
        assert_eq!(map_type_name("timestamp without time zone"), (Some(Ty::Timestamp), false));
        assert_eq!(map_type_name("timestamp(3)"), (Some(Ty::Timestamp), true));
        assert_eq!(map_type_name("date"), (Some(Ty::Date), false));
        assert_eq!(map_type_name("time"), (Some(Ty::Time), false));
        assert_eq!(map_type_name("interval"), (Some(Ty::Interval), false));
        assert_eq!(map_type_name("interval day to second"), (Some(Ty::Interval), true));
        assert_eq!(map_type_name("time with time zone").0, None);
        assert_eq!(map_type_name("timetz").0, None);
        assert_eq!(map_type_name("numeric(10,2)"), (Some(Ty::Real), true));
        // A type a dialect does not recognise comes back as a user-defined one and prints the way it
        // was written, so the quoted spelling has to reach the same entry the bare one does. Treating
        // it as unmappable would refuse the pair over punctuation.
        assert_eq!(map_type_name("\"text\""), map_type_name("text"));
        assert_eq!(map_type_name("\"text\"").0, Some(Ty::Str));
    }

    #[test]
    fn name_heuristics_order_boolean_before_id() {
        assert_eq!(name_type("is_deleted"), Some(Ty::Bool));
        assert_eq!(name_type("isActive"), Some(Ty::Bool));
        assert_eq!(name_type("deleted"), Some(Ty::Bool));
        assert_eq!(name_type("user_id"), Some(Ty::Int));
        assert_eq!(name_type("id"), Some(Ty::Int));
        // No guess for a temporal-sounding name: which temporal type it is decides its conversions.
        assert_eq!(name_type("created_at"), None);
        assert_eq!(name_type("start_date"), None);
        assert_eq!(name_type("first_name"), Some(Ty::Str));
        assert_eq!(name_type("qty"), None);
    }

    #[test]
    fn inferred_tables_are_nullable_and_keyless() {
        let mut uf = Uf::new();
        uf.set_type(&Atom::Col("t".into(), "x".into()), Ty::Int, Conf::Cast).unwrap();
        let mut refd = HashMap::new();
        refd.insert("t".to_string(), BTreeSet::from(["x".to_string(), "zz".to_string()]));
        let all = BTreeSet::from(["t".to_string()]);
        let cat = build_inferred(&all, &refd, &mut uf).unwrap();
        let t = &cat.tables[0];
        assert_eq!(t.cols, vec![("x".into(), "INTEGER".into()), ("zz".into(), "VARBINARY".into())]);
        assert_eq!(t.nullable, vec![true, true]);
        assert!(t.keys.is_empty(), "inventing a key could shrink the instance space");
    }

    // --- attribution -------------------------------------------------------

    #[test]
    fn bare_column_attaches_to_the_sole_base_table() {
        assert_eq!(
            schema_of("SELECT x FROM t WHERE user_id = 1"),
            [
                ("t".into(), "user_id".into(), "INTEGER".into()),
                ("t".into(), "x".into(), "VARBINARY".into()),
            ]
        );
    }

    #[test]
    fn bare_column_over_two_base_tables_refuses() {
        // Guessing which table owns `x` would put a column on a table that never had one, so the
        // synthesized schema would be wrong rather than merely imprecise.
        assert!(err_of("SELECT x FROM t, u").contains("ambiguous unqualified column"));
    }

    #[test]
    fn qualified_column_follows_the_alias() {
        assert_eq!(
            schema_of("SELECT a.x FROM tbl a WHERE a.x = 'abc'"),
            [("tbl".into(), "x".into(), "VARCHAR".into())]
        );
    }

    #[test]
    fn derived_source_columns_are_left_unresolved() {
        // `d.x` names a subquery's output, not a base column; only the inner `y` reaches `t`.
        assert_eq!(
            schema_of("SELECT d.x FROM (SELECT y AS x FROM t) d WHERE d.x = 'a'"),
            [("t".into(), "y".into(), "VARBINARY".into())]
        );
    }

    #[test]
    fn union_branches_get_separate_scopes() {
        // One `Query`, two SELECTs. Merging their FROM clauses into one scope would make both `x`
        // and `y` ambiguous; tracking scope per SELECT is what avoids that.
        assert_eq!(
            schema_of("SELECT x FROM t UNION SELECT y FROM u"),
            [
                ("t".into(), "x".into(), "VARBINARY".into()),
                ("u".into(), "y".into(), "VARBINARY".into()),
            ]
        );
    }

    #[test]
    fn correlated_qualifier_resolves_in_the_enclosing_scope() {
        assert_eq!(
            schema_of("SELECT 1 FROM t WHERE EXISTS (SELECT 1 FROM u WHERE u.k = t.id)"),
            [("t".into(), "id".into(), "INTEGER".into()), ("u".into(), "k".into(), "INTEGER".into())]
        );
    }

    #[test]
    fn a_base_table_with_no_attributed_column_refuses() {
        let e = err_of("SELECT a.x FROM a, b");
        assert!(e.contains("table without referenced columns: b"), "{e}");
    }

    // --- evidence ----------------------------------------------------------

    #[test]
    fn a_type_crosses_a_join_predicate() {
        // `a.k = b.k` unions the two atoms without saying what they are; the literal then types both.
        assert_eq!(
            schema_of("SELECT 1 FROM a JOIN b ON a.k = b.k WHERE b.k = 'x'"),
            [("a".into(), "k".into(), "VARCHAR".into()), ("b".into(), "k".into(), "VARCHAR".into())]
        );
    }

    #[test]
    fn a_cast_parameter_types_what_it_is_compared_to() {
        assert_eq!(
            schema_of("SELECT x FROM t WHERE x = $1::integer"),
            [("t".into(), "x".into(), "INTEGER".into())]
        );
    }

    #[test]
    fn a_substituted_qpn_call_is_the_same_atom_as_a_placeholder() {
        // The preprocessor rewrites `$1` to `qp1(0)`, so reading its output has to recognise both.
        // Here the parameter is typed on one side of the query and reaches `t.x` on the other.
        assert_eq!(
            schema_of("SELECT 1 FROM t WHERE x = qp1(0) AND qp1(0) = 'a'"),
            [("t".into(), "x".into(), "VARCHAR".into())]
        );
    }

    #[test]
    fn a_where_condition_is_boolean_context() {
        // No name evidence at all for `flag`; without the WHERE rule it would be VARBINARY, and a
        // predicate over an uninterpreted sort is refused downstream for lack of evidence.
        assert_eq!(
            schema_of("SELECT 1 FROM t WHERE flag"),
            [("t".into(), "flag".into(), "BOOLEAN".into())]
        );
    }

    #[test]
    fn an_unmappable_cast_target_contributes_no_type() {
        // It used to refuse. What an unmappable target denies us is the *cast's* interpretation, not
        // the operand's type, and the cast rewrite models exactly that as a `qcast` symbol -- so this
        // pass now stays silent about the cast's result and lets the rest of the query speak, which is
        // what an untyped column already does. `y` is typed by nothing, so it lands on the default.
        assert_eq!(
            schema_of("SELECT 1 FROM t WHERE x = y::jsonb"),
            vec![
                ("t".to_string(), "x".to_string(), "VARBINARY".to_string()),
                ("t".to_string(), "y".to_string(), "VARBINARY".to_string()),
            ]
        );
        // And a *typed* operand keeps its type: the cast is what is unreadable, not the column.
        assert_eq!(
            schema_of("SELECT 1 FROM t WHERE t.n + 1 > 0 AND t.n::int[] IS NOT NULL"),
            vec![("t".to_string(), "n".to_string(), "INTEGER".to_string())]
        );
    }

    #[test]
    fn cte_input_is_refused_rather_than_mis_attributed() {
        let e = err_of("WITH c AS (SELECT x FROM t) SELECT x FROM c");
        assert!(e.contains("WITH (CTE)"), "{e}");
    }

    /// `f(0)."id"` selects a field out of a call's result. sqlparser models the `.id` as a real
    /// `Expr::Identifier` hanging off `CompoundFieldAccess`, so without the guard the walk would
    /// treat it as a bare column and give `t` an `id` it never had.
    #[test]
    fn a_field_selected_from_a_call_is_not_a_column() {
        assert_eq!(
            schema_of("SELECT 1 FROM t WHERE t.thread_id = qp2(0).\"id\""),
            [("t".into(), "thread_id".into(), "INTEGER".into())]
        );
    }

    #[test]
    fn both_sides_of_a_pair_share_one_type_environment() {
        // The left query states the type; the right one only mentions the column. If each side got
        // its own union-find they would be handed two different schemas.
        let sql = "SELECT x FROM t WHERE x = 1; SELECT x FROM t;";
        assert_eq!(schema_of(sql), [("t".into(), "x".into(), "INTEGER".into())]);
    }
}
