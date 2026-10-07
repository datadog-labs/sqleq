// Unless explicitly stated otherwise all files in this repository are licensed under the
// Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/).
// Copyright 2026-Present Datadog, Inc.

//! Name resolution scope and de-Bruijn indexing.
//!
//! The prover references columns by an absolute *variable level* (de-Bruijn index) into the
//! flattened row scope, not by name — and the numbering is absolute across query nesting (the prover
//! evaluates a subquery with the outer row variables already in scope). So every binding carries its
//! absolute column `offset`: in a correlated subquery the outer row occupies `[0, outer_width)` and
//! the subquery's own columns `[outer_width, ...)`.
//!
//! Names are stored and looked up as Postgres identifies them: an unquoted identifier folds to lower
//! case and a quoted one keeps its case (see [`crate::dml::fold_ident`]), so `A`, `a` and `"a"` are one
//! name and `"A"` is another. Every name a [`Binding`] holds, and every name a caller passes to
//! [`Scope::try_resolve`], is already in that form.

/// One relation instance in a FROM clause (a base table or a derived subquery), with its absolute
/// column offset in the row scope and its output columns `(name, type)`.
#[derive(Clone)]
pub struct Binding {
    pub alias: String,
    pub cols: Vec<(String, String)>,
    pub offset: usize,
    /// Catalog index of the table this binding scans, or `None` for a derived table.
    ///
    /// Needed to reach the declared keys of the *instance* a column came from, which is what makes
    /// the GROUP BY functional-dependence rule safe under self-joins: two aliases of one table are
    /// two bindings, so a key grouped on one of them says nothing about a column read from the
    /// other. Matching on the table's name instead would conflate them.
    pub table: Option<usize>,
    /// How many of `cols` are visible to `*` — the declared prefix. See [`crate::catalog::Table`]'s
    /// field of the same name: the trailing entries are Postgres system columns, which occupy a
    /// de-Bruijn level like any other column but are not part of any relation's output shape.
    ///
    /// Equal to `cols.len()` for a derived table, whose columns *are* its output.
    pub n_declared: usize,
}

/// A resolution scope. `binds[0..inner_count]` are this query's own FROM bindings; `binds[inner_count..]`
/// are enclosing-query bindings, present only so correlated references resolve. Inner bindings come
/// first so name lookup shadows correctly.
pub struct Scope {
    pub binds: Vec<Binding>,
    pub inner_count: usize,
    /// Where this query's own row starts in the level numbering — the total width of the enclosing
    /// context, i.e. `outer_width(outer)`.
    ///
    /// `Binding::offset` already accounts for it, so a *column* reference never needs it. It is
    /// needed for the indices we mint against a binder the frontend introduces itself rather than
    /// one that came from a FROM item: the pre-aggregation projection, the group output, and the
    /// projection under `SELECT DISTINCT`. The prover evaluates those in `subst ++ source_scope`,
    /// where `subst` is the enclosing row — so the *n*th column of such a binder is level
    /// `base + n`, and writing plain `n` reaches an enclosing column instead. Zero at the top
    /// level, which is why that mistake stays invisible until the query is a subquery.
    pub base: usize,
    /// Column names (folded, see the module docs) that a `JOIN ... USING` merged into a single
    /// output column.
    ///
    /// `USING` does two things: it adds the equalities, and it collapses each named pair into one
    /// output column. We model only the first. The second is why these names are tracked: a bare
    /// `*` over a `USING` join has one column where the `ON` form has two, and under an outer join
    /// an unqualified reference to a merged name is the *outer* side's value (or, for `FULL`, a
    /// coalesce of both) rather than whichever binding happens to come first. Both are refused
    /// rather than silently lowered as the `ON` form, which would equate two different queries.
    pub merged: Vec<String>,
    /// Whether any merged name is unsafe to reach unqualified — true once a non-`INNER` join uses
    /// `USING`. Under an inner join the merged column equals both sides (the equality holds and
    /// excludes NULLs), so resolving to either is faithful.
    pub merged_outer: bool,
    /// The merged names a `RIGHT` or `FULL` join merged. Each is a coalesce of its two sides, which
    /// no one binding holds; after a `LEFT` join the merged column is the left side's, which is the
    /// first binding with that name. A further `USING` over one of these is refused.
    pub coalesced: Vec<String>,
}

impl Scope {
    /// An empty scope (e.g. for VALUES, whose expressions are constants).
    pub fn empty() -> Self {
        Scope {
            binds: Vec::new(),
            inner_count: 0,
            base: 0,
            merged: Vec::new(),
            merged_outer: false,
            coalesced: Vec::new(),
        }
    }

    /// The binding an absolute column level falls in, and the level's index within it.
    ///
    /// Searches every binding, inner and outer alike: a correlated reference resolves to an
    /// enclosing binding, and a key grouped on that same binding determines it just as it would in
    /// the local case.
    pub fn binding_of(&self, level: usize) -> Option<(&Binding, usize)> {
        self.binds
            .iter()
            .find(|b| level >= b.offset && level < b.offset + b.cols.len())
            .map(|b| (b, level - b.offset))
    }

    /// Whether this reference reaches a `USING`-merged column whose value we do not model.
    pub fn merged_conflict(&self, qual: Option<&str>, col: &str) -> bool {
        self.merged_outer && qual.is_none() && self.merged.iter().any(|m| m == col)
    }

    /// This query's own bindings (excluding enclosing/correlation bindings).
    pub fn inner(&self) -> &[Binding] {
        &self.binds[..self.inner_count]
    }

    /// This scope cut back to its first `n` own bindings; enclosing bindings are kept, so
    /// correlation still resolves.
    ///
    /// A join's `ON` condition is the one place where less than the whole FROM clause is in scope.
    /// The prover evaluates it in `subst ++ left ++ right`, so for the left-deep chain we emit that
    /// is the bindings up to and including this join and no more. Two things go wrong when the
    /// condition is lowered against the finished scope instead: a reference to a table further right
    /// resolves (SQL says it should not) to an index past the end of the join's row, and — the way
    /// this was found — a *subquery* in the condition numbers its own columns after the whole FROM
    /// clause rather than after this join, putting every one of them out of range.
    ///
    /// `merged`/`merged_outer` are carried over whole rather than recomputed for the prefix. They
    /// only ever cause refusals, so the worst a stale entry does is refuse a `USING` name one join
    /// too early.
    pub fn prefix(&self, n: usize) -> Scope {
        let mut binds: Vec<Binding> = self.binds[..n].to_vec();
        binds.extend(self.binds[self.inner_count..].iter().cloned());
        Scope {
            binds,
            inner_count: n,
            base: self.base,
            merged: self.merged.clone(),
            merged_outer: self.merged_outer,
            coalesced: self.coalesced.clone(),
        }
    }

    /// Total column width of an enclosing context (the base offset for a nested query's bindings).
    pub fn outer_width(outer: &[Binding]) -> usize {
        outer.iter().map(|b| b.cols.len()).sum()
    }

    /// Resolve a column reference to its absolute de-Bruijn index and type. `qual` and `col` are
    /// folded names (see the module docs). Searches inner bindings first (SQL shadowing), then outer
    /// bindings (correlation). Returns `None` if unresolved anywhere — the caller then refuses it
    /// rather than silently rebinding (a soundness rule).
    pub fn try_resolve(&self, qual: Option<&str>, col: &str) -> Option<(usize, String)> {
        if let Some(q) = qual {
            if !self.binds.iter().any(|b| b.alias == q) {
                return None;
            }
        }
        for b in &self.binds {
            if qual.is_some_and(|q| b.alias != q) {
                continue;
            }
            if let Some(i) = b.cols.iter().position(|(n, _)| n == col) {
                return Some((b.offset + i, b.cols[i].1.clone()));
            }
        }
        None
    }

    /// Whether any inner binding carries columns `*` must not expand to — the system columns
    /// [`crate::catalog::add_system_columns`] appended.
    ///
    /// False for every pair that names no system column, which is what keeps the
    /// `SELECT *`-is-the-scan shortcut in [`crate::lower`] byte-identical for the rest of the corpus.
    pub fn hides_columns(&self) -> bool {
        self.inner().iter().any(|b| b.n_declared != b.cols.len())
    }

    /// This query's own output columns `(name, type)` — inner bindings only (correlated outer
    /// columns are not part of `SELECT *`), and each binding's declared prefix only (a system
    /// column is readable by name but is not part of the shape a derived table exposes).
    pub fn out_cols(&self) -> Vec<(String, String)> {
        self.inner().iter().flat_map(|b| b.cols[..b.n_declared].iter().cloned()).collect()
    }
}
