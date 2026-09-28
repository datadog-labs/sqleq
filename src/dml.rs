// Unless explicitly stated otherwise all files in this repository are licensed under the
// Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/).
// Copyright 2026-Present Datadog, Inc.

//! `DELETE` and `UPDATE` reduced to the `SELECT` that computes their effect.
//!
//! The prover's IR has relations and no statements: nothing in it mutates a table, so a pair of
//! `DELETE`s or `UPDATE`s cannot be handed to it as written. What *can* be handed to it is a query
//! computing the thing the two statements have to agree on, which turns "are these two statements
//! equivalent" into "are these two queries bag-equivalent" — the question the prover already answers.
//! Three shapes occur in practice and all three are handled: both sides a bare `DELETE`/`UPDATE`, one
//! side wrapped in a `WITH`, and the two mixed across the pair.
//!
//! Like the rewrites in [`normalize`][crate::normalize] this is a rewrite whose verdict is reported
//! for the *original* pair, so an unsound reduction does not fail, it lies. Each of the two carries
//! its equivalence argument below and every precondition of that argument is a guard in the code.
//!
//! ## `DELETE`
//!
//! ```text
//! DELETE FROM T WHERE P   ~>   SELECT * FROM T WHERE P
//! ```
//!
//! The statement's whole effect is `T := T - D`, where `D` is the bag it deletes and `D`'s
//! multiplicity for a row `r` is `mult_T(r) * [P(r)]` — exactly what the `SELECT` computes. Bag
//! subtraction is cancellative, so `T - D₁ = T - D₂` for every `T` iff `D₁ = D₂`: the reduced pair is
//! equivalent exactly when the original one is. That is an *iff*, so unusually for this frontend the
//! rewrite loses nothing rather than being conservative in one direction.
//!
//! ## `UPDATE`, and the columns that must be projected even though neither side assigns them
//!
//! ```text
//! UPDATE T SET c = e WHERE P   ~>   SELECT CASE WHEN P THEN e ELSE c END AS c, d AS d, … FROM T
//! ```
//!
//! An `UPDATE` adds and removes no rows; it replaces each row with a row over the same columns. So
//! the final table is a projection of the original, one output column per column of `T`: the assigned
//! expression where `P` holds, the old value where it does not. `WHERE P` matches a row only when `P`
//! is TRUE, and `CASE WHEN P THEN e ELSE c END` takes its `ELSE` branch when `P` is FALSE *or* NULL —
//! the same partition of the rows, so the encoding is faithful in the three-valued case too. Every
//! `SET` expression reads the pre-update row, which is what the projection is over, so nothing needs
//! sequencing.
//!
//! **Every column of `T` is projected, not just the ones some side assigns.** The tempting shortcut
//! is to project the union of the two sides' `SET` columns, on the argument that a column touched by
//! neither update is identical on both sides and can be omitted. It is unsound: such a column *is*
//! identical row by row — but the projection is compared as a **bag**, and dropping a column drops
//! the pairing between the columns kept and the ones omitted:
//!
//! ```text
//! T(a, b) = {(1, 2), (2, 1)}
//! A: UPDATE t SET a = 3 - a     leaves T = {(2, 2), (1, 1)}
//! B: UPDATE t SET a = a         leaves T = {(1, 2), (2, 1)}
//! ```
//!
//! Projected on `a` alone, A is the bag `{2, 1}` and B is the bag `{1, 2}` — equal, so the pair is
//! proved, while the two statements leave visibly different tables. Projecting `b` as well separates
//! them. The price is that the reduction needs `T`'s full column list and therefore the catalog, so an
//! `UPDATE` of a table the input does not declare is refused rather than reduced.
//!
//! ## `RETURNING` is droppable on a `DELETE` and not on an `UPDATE`
//!
//! A statement with `RETURNING` has two observable results, the new table and the returned bag, and
//! the reduction has to account for both. For a `DELETE` the deleted bag determines both — the
//! surviving table is `T - D` and the returned rows are a projection of `D` — so two `DELETE`s that
//! delete the same bag and carry the *same* `RETURNING` list agree on both, and the clause can be
//! dropped. See [`same_returning`].
//!
//! For an `UPDATE` it cannot, even when the two clauses are identical, because what an `UPDATE`
//! returns is a projection of the rows its `WHERE` matched — and final-table equality does not pin
//! that down:
//!
//! ```text
//! A: UPDATE t SET c = c WHERE true  RETURNING c     leaves T unchanged, returns every row
//! B: UPDATE t SET c = c WHERE false RETURNING c     leaves T unchanged, returns nothing
//! ```
//!
//! Both are no-ops on the table, so the reduced projections are equivalent, while the statements
//! return different bags. (The preprocessor ignores the clause on both, and so gets this wrong.)
//! The `UPDATE` reduction therefore emits **two** goals rather than one.
//!
//! ## Two goals in one query: the tagged `UNION ALL`
//!
//! [`collect_queries`][crate::catalog::collect_queries] wants exactly two queries and [`reduce`]
//! installs exactly one per side, so a conjunction of two goals has to live inside a single query.
//! It does, as a union tagged with which goal each block is — see [`two_goals`]:
//!
//! ```text
//!           SELECT 0 AS q_goal, <the new row> FROM t
//! UNION ALL SELECT 1 AS q_goal, <the new row> FROM t WHERE <pred>
//! ```
//!
//! Block 0 is the reduction above, unchanged. Block 1 is the rows the `WHERE` matched, holding the
//! values they end up with — which is what an `UPDATE`'s `RETURNING` hands back, before the list
//! projects it. `q_goal` partitions the union, so bag equality of the whole is bag equality of
//! block 0 *and* of block 1 — **provided the prover can tell `0` from `1`**. If it could not, the
//! two goals could cross-cancel (side A's block 0 against side B's block 1 and back), and that is
//! a false proof rather than a lost one, so it has its own adversarial test.
//!
//! **Block 1 is deliberately stronger than the returned bag.** It is the matched rows at full
//! width, not the `RETURNING` list applied to them. For any list `L` the two sides share,
//! `matched_A = matched_B` implies `L(matched_A) = L(matched_B)`, so proving this goal proves the
//! one that was asked — but not conversely, since `L` may drop the column that separates them.
//! The reduction is an iff everywhere else in this module; here it holds in one direction only, so
//! a pair can be equivalent and unproved, never inequivalent and proved. What it buys is that
//! nothing has to resolve the list, type it, or expand a `*`: the list is only ever *compared
//! between the two sides*, never evaluated, so `RETURNING $4` costs no more than `RETURNING id`.
//! The alternative — project `L` into a second pair of slots and pad the other block with typed
//! NULLs — cannot be typed at all for a column whose declared type is outside the five the prover
//! has, because [`casts`][crate::casts] retargets `CAST(NULL AS uuid)` to `VARBINARY` while the
//! column itself lowers under its catalog spelling.
//!
//! The two sides' lists still have to denote the same projection, and [`same_returning`] decides
//! that on a canonical form rather than on the syntax: `*` and `s.*` are one list when `s` names
//! the target, and so are `id` and `u.id`. Most `RETURNING`-bearing pairs agree under that folding;
//! the rest either differ genuinely or carry the clause on one side only, and both are refused — the
//! latter matching the guard `sqleq-fuzz` applies.
//!
//! ## Both sides have to be the same kind of statement
//!
//! A `DELETE` is reduced only against another `DELETE` and an `UPDATE` only against another `UPDATE`.
//! The preprocessor reduces a `DELETE` on its own and then compares it with whatever the other side
//! is, so a `DELETE` paired with a plain `SELECT` becomes "deleted bag vs. query result" and a
//! statement that mutates a table can be reported equivalent to one that reads it. The `UPDATE`
//! reduction is *jointly* defined anyway (both sides project the same column list), so it needs the
//! pair regardless.
//!
//! ## Where this runs
//!
//! Between [`fix_precedence`][crate::normalize::fix_precedence] and every normalization. After the
//! repair, because the reduction copies the `WHERE` predicate into one `CASE` per column and the tree
//! it copies has to be the one the SQL means; before the normalizations, because then no other pass
//! needs to know that DML exists — everything downstream of here sees two queries.

use sqlparser::ast::helpers::attached_token::AttachedToken;
use sqlparser::ast::{
    CaseWhen, Delete, Expr, FromTable, Ident, Insert, ObjectName, Query, SelectItem,
    SelectItemQualifiedWildcardKind, SetExpr, Statement, TableFactor, TableObject, TableWithJoins,
    Update, WildcardAdditionalOptions,
};
use sqlparser::parser::Parser;

use crate::catalog::{obj_name, Catalog};
use crate::error::{schema, unsupported, Result};

/// Replace every top-level `DELETE`/`UPDATE`/`INSERT` in `statements` with the query that computes
/// its effect.
///
/// Leaves anything else alone, and is refused downstream for not being a query. For a `MERGE` that
/// is because there is no such reduction: its effect depends on which rows already match.
///
/// The three reductions are one per kind and a pair has to be all of one kind, because two
/// statements of different kinds do not have the same observable to compare.
///
/// How far the `INSERT` arm reaches is decided by what real pairs hold rather than by the theory.
/// Surveying `INSERT`/`INSERT` pairs and bucketing them -- mutually exclusive buckets, ordered the
/// way [`insert_pair`] meets them -- almost all of them land above the reduction rather than in it:
///
/// - `ON CONFLICT` on a side, much the largest bucket -- upsert is not bag addition.
/// - one side spelled `VALUES` and the other `SELECT * FROM unnest(..)`, nearly as large.
/// - `RETURNING` on a side, which is a second observable.
/// - the two sides given different column lists, a handful.
/// - a small remainder, and on that remainder the reduction applies.
///
/// The `unnest` bucket is the one that looks like the prize and is not: the two sides share a `$N`
/// bound to n scalars on one side and to one array on the other, and the index binding contract has
/// exactly one value per `$N` for both sides, so there is no correspondence to quantify over.
/// Fixing `param_cols` there moves the error, not the verdict. And the remainder shrinks again
/// downstream: DuckDB's binder rejects part of it, and of what does bind, some is skipped as
/// nondeterministic and some inserts NULL into a NOT NULL column because we do not model `DEFAULT`.
pub fn reduce(cat: &Catalog, statements: &mut [Statement]) -> Result<()> {
    let deletes: Vec<usize> =
        (0..statements.len()).filter(|&i| as_delete(&statements[i]).is_some()).collect();
    let updates: Vec<usize> =
        (0..statements.len()).filter(|&i| as_update(&statements[i]).is_some()).collect();
    let inserts: Vec<usize> =
        (0..statements.len()).filter(|&i| as_insert(&statements[i]).is_some()).collect();

    match deletes.len() {
        0 => {}
        2 => {
            let (i, j) = (deletes[0], deletes[1]);
            let (a, b) = (delete_at(statements, i), delete_at(statements, j));
            let (da, db) = (delete_target(a)?, delete_target(b)?);
            let (qa, qb) = (target_qual(da)?, target_qual(db)?);
            same_returning(a.returning.as_deref(), b.returning.as_deref(), &qa, &qb, "DELETE")?;
            same_target(da, db)?;
            let (ra, rb) = (delete_to_select(a)?, delete_to_select(b)?);
            install(&mut statements[i], ra)?;
            install(&mut statements[j], rb)?;
        }
        // Both sides have to be a `DELETE`; see the module docs. This also covers an input carrying
        // three of them, which is not a pair whatever else it is.
        _ => return Err(unsupported("DELETE paired with non-DELETE")),
    }

    match updates.len() {
        0 => {}
        2 => {
            let (i, j) = (updates[0], updates[1]);
            let (qa, qb) = update_pair(cat, update_at(statements, i), update_at(statements, j))?;
            install(&mut statements[i], qa)?;
            install(&mut statements[j], qb)?;
        }
        _ => return Err(unsupported("UPDATE paired with non-UPDATE")),
    }

    match inserts.len() {
        0 => {}
        2 => {
            let (i, j) = (inserts[0], inserts[1]);
            let (qa, qb) = insert_pair(cat, insert_at(statements, i), insert_at(statements, j))?;
            install(&mut statements[i], qa)?;
            install(&mut statements[j], qb)?;
        }
        _ => return Err(unsupported("INSERT paired with non-INSERT")),
    }
    Ok(())
}

/// The `DELETE` a statement is, if it is one.
///
/// Two shapes reach here. A bare `DELETE` is a [`Statement::Delete`]; one under a `WITH` is a
/// [`Statement::Query`] whose body is a [`SetExpr::Delete`], because the bindings attach to a query
/// wrapper rather than to the statement. Both are the same reduction, so both are found here and
/// [`install`] puts the wrapper's bindings back.
fn as_delete(st: &Statement) -> Option<&Delete> {
    match st {
        Statement::Delete(d) => Some(d),
        Statement::Query(q) => match &*q.body {
            SetExpr::Delete(Statement::Delete(d)) => Some(d),
            _ => None,
        },
        _ => None,
    }
}

/// The `UPDATE` a statement is, if it is one. See [`as_delete`] for the two shapes.
fn as_update(st: &Statement) -> Option<&Update> {
    match st {
        Statement::Update(u) => Some(u),
        Statement::Query(q) => match &*q.body {
            SetExpr::Update(Statement::Update(u)) => Some(u),
            _ => None,
        },
        _ => None,
    }
}

/// The `INSERT` a statement is, if it is one. See [`as_delete`] for the two shapes.
fn as_insert(st: &Statement) -> Option<&Insert> {
    match st {
        Statement::Insert(i) => Some(i),
        Statement::Query(q) => match &*q.body {
            SetExpr::Insert(Statement::Insert(i)) => Some(i),
            _ => None,
        },
        _ => None,
    }
}

fn delete_at(statements: &[Statement], i: usize) -> &Delete {
    as_delete(&statements[i]).expect("index collected from as_delete")
}

fn update_at(statements: &[Statement], i: usize) -> &Update {
    as_update(&statements[i]).expect("index collected from as_update")
}

fn insert_at(statements: &[Statement], i: usize) -> &Insert {
    as_insert(&statements[i]).expect("index collected from as_insert")
}

/// Swap a DML statement for the query that computes its effect, carrying any `WITH` across.
///
/// The bindings have to move rather than be discarded: [`inline_ctes`][crate::normalize::inline_ctes]
/// is what substitutes them into the uses the predicate and the projection now contain, and what
/// refuses a `RECURSIVE` or data-modifying one. Dropping them would leave those uses unresolved,
/// which is a refusal — but a refusal for the wrong reason, and only by luck rather than by design.
fn install(st: &mut Statement, mut reduced: Query) -> Result<()> {
    if let Statement::Query(wrapper) = st {
        if let Some(outer) = wrapper.with.take() {
            // Two sets of bindings and one slot to put them in. Only the `INSERT` reduction can
            // reach this, because it is the only one whose result is a query the input already
            // contained and so the only one that can arrive here carrying a `WITH` of its own;
            // merging the two would have to rename whatever names they share.
            if reduced.with.is_some() {
                return Err(unsupported("WITH on both the INSERT and its source"));
            }
            reduced.with = Some(outer);
        }
    }
    *st = Statement::Query(Box::new(reduced));
    Ok(())
}

/// `DELETE FROM T WHERE P` -> `SELECT * FROM T WHERE P`. See the module docs for the argument.
fn delete_to_select(d: &Delete) -> Result<Query> {
    // A join-delete deletes the rows of `T` that a semi-join with the `USING` relations keeps. That
    // is still a bag of `T`'s rows and the argument would still go through, but the projection is not
    // `SELECT * FROM T WHERE P` — 8 statements in the corpus, none of whose pairs lower anyway.
    if d.using.is_some() {
        return Err(unsupported("DELETE ... USING (join-delete)"));
    }
    // MySQL's `DELETE t1, t2 FROM …` deletes from several tables at once, so its effect is not one
    // bag and there is nothing for a single `SELECT` to compute.
    if !d.tables.is_empty() {
        return Err(unsupported("multi-table DELETE"));
    }
    // `ORDER BY … LIMIT` makes the deleted bag depend on which rows the ordering happened to put
    // first, and an ordering that does not totally order the table leaves that underdetermined.
    if !d.order_by.is_empty() || d.limit.is_some() {
        return Err(unsupported("DELETE with ORDER BY / LIMIT"));
    }
    // T-SQL's `OUTPUT` is `RETURNING` with more options (it can see both the old and new row and can
    // write them to a table); nothing in the corpus uses it and it is not analysed here.
    if d.output.is_some() {
        return Err(unsupported("DELETE ... OUTPUT"));
    }
    let target = delete_target(d)?;
    Ok(select(
        vec![SelectItem::Wildcard(WildcardAdditionalOptions::default())],
        target.clone(),
        d.selection.clone(),
    ))
}

/// The single table a `DELETE` deletes from.
fn delete_target(d: &Delete) -> Result<&TableWithJoins> {
    let (FromTable::WithFromKeyword(from) | FromTable::WithoutKeyword(from)) = &d.from;
    let [target] = from.as_slice() else {
        return Err(unsupported("DELETE from more than one relation"));
    };
    target_name(target)?;
    Ok(target)
}

/// Check a DML target is a single plain table and return its name.
///
/// Only the shape the reduction's own argument needs is checked here: one relation, no join, and a
/// table rather than a subquery or a table function. Every *other* property of the factor —
/// `TABLESAMPLE`, `FOR SYSTEM_TIME AS OF`, `PARTITION (…)`, `WITH ORDINALITY` — is carried across
/// unchanged into the reduced query, where [`lower`][crate::lower] already refuses each one. A second
/// copy of that list here would be a second copy to keep in step.
fn target_name(t: &TableWithJoins) -> Result<&ObjectName> {
    if !t.joins.is_empty() {
        return Err(unsupported("DML target with a join"));
    }
    match &t.relation {
        TableFactor::Table { name, .. } => Ok(name),
        _ => Err(unsupported("DML target is not a plain table")),
    }
}

/// Both statements of the pair must name the same table.
///
/// Two statements mutating two different tables are not two spellings of one statement, whatever
/// their reductions come out as. Compared on the qualified name and not on the bare one, which costs
/// nothing — no pair in the corpus spells its target two ways — and keeps this check independent of
/// what [`strip_schema`][crate::normalize::strip_schema] later decides about the qualifier.
fn same_target(a: &TableWithJoins, b: &TableWithJoins) -> Result<()> {
    let (na, nb) = (obj_name(target_name(a)?), obj_name(target_name(b)?));
    if na.to_lowercase() != nb.to_lowercase() {
        return Err(unsupported(format!("the pair's two statements target {na} and {nb}")));
    }
    Ok(())
}

/// Postgres's identity for an identifier: an unquoted name folds to lower case, a quoted one does
/// not.
///
/// This is the language's rule and not a normalisation of convenience, because it decides whether
/// two `RETURNING` lists are the same projection — folding too much is a false proof, folding too
/// little is a refusal.
fn fold_ident(id: &Ident) -> String {
    match id.quote_style {
        Some(_) => id.value.clone(),
        None => id.value.to_lowercase(),
    }
}

/// The one name a `RETURNING` item may qualify a column of the DML target with.
///
/// An alias hides the table's own name in Postgres, so it is the alias where there is one and the
/// bare table name otherwise — a `schema.t` target is referred to as `t`. A three-part
/// `schema.t.col` reference is left uncanonicalised by [`ret_item`] rather than resolved here; it
/// then compares structurally, which costs a refusal at worst.
fn target_qual(t: &TableWithJoins) -> Result<String> {
    if let TableFactor::Table { alias: Some(a), .. } = &t.relation {
        return Ok(fold_ident(&a.name));
    }
    let name = target_name(t)?;
    // A part that is not an identifier leaves the empty string, which no real qualifier equals.
    Ok(name.0.last().and_then(|p| p.as_ident()).map(fold_ident).unwrap_or_default())
}

/// A `RETURNING` item reduced to *what it projects*, so the two sides compare on meaning rather
/// than on syntax.
///
/// Only the shapes whose meaning is decidable from the item alone are folded; everything else keeps
/// the item and compares structurally, which is what this check did for `DELETE` when structural
/// equality was all it had. So no pair the old comparison accepted is rejected by this one.
/// `AttachedToken`'s `PartialEq` is constant-true, which is what keeps [`Self::Other`] insensitive
/// to spans and formatting and sensitive to everything else.
#[derive(PartialEq, Debug)]
enum RetItem<'a> {
    /// `*`, or `q.*` where `q` names the target. The target is the only relation in scope — a
    /// `USING`/`FROM` join is refused before this runs — so both expand to its declared columns,
    /// in declared order.
    Star,
    /// A column of the target, written bare or qualified by the target's name or alias.
    Col(String),
    /// Anything else: a parameter, a call, an aliased expression, a wildcard carrying `EXCEPT` or
    /// `REPLACE`. Equal only to a structurally identical item.
    Other(&'a SelectItem),
}

fn ret_item<'a>(item: &'a SelectItem, qual: &str) -> RetItem<'a> {
    let plain = |o: &WildcardAdditionalOptions| *o == WildcardAdditionalOptions::default();
    match item {
        SelectItem::Wildcard(o) if plain(o) => RetItem::Star,
        SelectItem::QualifiedWildcard(SelectItemQualifiedWildcardKind::ObjectName(n), o)
            if plain(o)
                && n.0.len() == 1
                && n.0[0].as_ident().map(fold_ident).as_deref() == Some(qual) =>
        {
            RetItem::Star
        }
        // A bare identifier in a `RETURNING` list can only be a column of the target: the clause
        // has no outer scope to correlate with.
        SelectItem::UnnamedExpr(Expr::Identifier(c)) => RetItem::Col(fold_ident(c)),
        SelectItem::UnnamedExpr(Expr::CompoundIdentifier(parts)) => match parts.as_slice() {
            [q, c] if fold_ident(q) == qual => RetItem::Col(fold_ident(c)),
            _ => RetItem::Other(item),
        },
        _ => RetItem::Other(item),
    }
}

/// Whether the pair carries a `RETURNING` clause at all, refusing one that is one-sided or that the
/// two sides do not spell to the same projection.
///
/// One-sided is refused because the two statements then do not have the same observable to begin
/// with; `sqleq-fuzz` applies the same guard, so the two axes agree by construction. Differing is
/// refused because the goal a shared list licenses — on a `DELETE`, dropping the clause; on an
/// `UPDATE`, [`two_goals`] — compares the affected rows and not the list, which pins the returned
/// bags down only when the list is the same on both sides.
fn same_returning(
    a: Option<&[SelectItem]>,
    b: Option<&[SelectItem]>,
    qa: &str,
    qb: &str,
    kind: &str,
) -> Result<bool> {
    let (a, b) = match (a, b) {
        (None, None) => return Ok(false),
        (Some(a), Some(b)) => (a, b),
        _ => return Err(unsupported(format!("{kind} ... RETURNING on one side only"))),
    };
    let ia: Vec<RetItem<'_>> = a.iter().map(|i| ret_item(i, qa)).collect();
    let ib: Vec<RetItem<'_>> = b.iter().map(|i| ret_item(i, qb)).collect();
    if ia != ib {
        return Err(unsupported(format!(
            "{kind} ... RETURNING that differs between the two queries"
        )));
    }
    Ok(true)
}

/// The parts of an `UPDATE` the reduction reads: its target, its assignments as
/// `(lowercased column, value)`, and its predicate. All borrowed from the statement, because the
/// value expressions are spliced into the projection unchanged.
type Parts<'a> = (&'a TableWithJoins, Vec<(String, &'a Expr)>, Option<&'a Expr>);

/// Read an `UPDATE` into its [`Parts`], refusing every shape the projection does not model.
fn set_map(u: &Update) -> Result<Parts<'_>> {
    // `UPDATE t SET c = u.c FROM u WHERE …` is a join-update: which row of `u` supplies the value is
    // decided by the join, and a row of `t` matching several of them updates once with an
    // unspecified one of them. That is not a projection of `t`.
    if u.from.is_some() {
        return Err(unsupported("UPDATE ... FROM (join-update)"));
    }
    if !u.order_by.is_empty() || u.limit.is_some() {
        return Err(unsupported("UPDATE with ORDER BY / LIMIT"));
    }
    if u.output.is_some() {
        return Err(unsupported("UPDATE ... OUTPUT"));
    }
    // SQLite's `UPDATE OR IGNORE|REPLACE|…` changes what happens when the new row violates a
    // constraint, which is an effect on the final table that the projection does not model.
    if u.or.is_some() {
        return Err(unsupported("UPDATE OR <conflict clause>"));
    }
    target_name(&u.table)?;

    let mut sets: Vec<(String, &Expr)> = Vec::new();
    for a in &u.assignments {
        let sqlparser::ast::AssignmentTarget::ColumnName(c) = &a.target else {
            // `SET (a, b) = (SELECT …)` assigns a row value: each target takes one column of a
            // subquery whose cardinality SQL constrains but the tree does not, so splitting it into
            // per-column expressions is not a rewrite of this shape. 4 statements in the corpus.
            return Err(unsupported("UPDATE SET (a, b) = ... (row assignment)"));
        };
        // Postgres does not allow the target to be qualified, and a multi-part name here would name a
        // column of something other than the table being projected.
        let [part] = c.0.as_slice() else {
            return Err(unsupported("qualified SET target"));
        };
        let Some(id) = part.as_ident() else {
            return Err(unsupported("SET target is not an identifier"));
        };
        let col = id.value.to_lowercase();
        // Two assignments to one column: Postgres rejects it, and the projection has one slot per
        // column so it could only keep one of them.
        if sets.iter().any(|(seen, _)| *seen == col) {
            return Err(unsupported(format!("SET assigns {col} twice")));
        }
        sets.push((col, &a.value));
    }
    if sets.is_empty() {
        return Err(unsupported("UPDATE with no SET clause"));
    }
    Ok((&u.table, sets, u.selection.as_ref()))
}

/// Reduce a pair of `UPDATE`s to the pair of projections computing their final tables.
fn update_pair(cat: &Catalog, a: &Update, b: &Update) -> Result<(Query, Query)> {
    let (ta, sets_a, pred_a) = set_map(a)?;
    let (tb, sets_b, pred_b) = set_map(b)?;
    same_target(ta, tb)?;
    let returning = same_returning(
        a.returning.as_deref(),
        b.returning.as_deref(),
        &target_qual(ta)?,
        &target_qual(tb)?,
        "UPDATE",
    )?;

    let name = obj_name(target_name(ta)?);
    let idx = find_target(cat, &name)
        .ok_or_else(|| schema(format!("no declared schema for UPDATE target {name}")))?;
    // The declared prefix: an `UPDATE` projects the table's own shape, and a system column
    // `catalog::add_system_columns` appended would change its arity. Independent of the fact that
    // the reduction currently runs before that append, so moving either cannot break this.
    let t = &cat.tables[idx];
    let cols: Vec<String> = t.cols[..t.n_declared].iter().map(|(c, _)| c.clone()).collect();
    // An assignment to a column the catalog does not have would simply not appear in the
    // projection — the update would become invisible. Refuse rather than drop it.
    for (col, _) in sets_a.iter().chain(&sets_b) {
        if !cols.contains(col) {
            return Err(schema(format!("UPDATE sets {col}, which {name} does not declare")));
        }
    }

    let (pa, pb) = (project(&cols, &sets_a, pred_a), project(&cols, &sets_b, pred_b));
    // Without a `RETURNING` clause the final table is the whole observable and one goal says it all.
    if !returning {
        return Ok((select(pa, ta.clone(), None), select(pb, tb.clone(), None)));
    }
    Ok((two_goals(pa, ta, pred_a), two_goals(pb, tb, pred_b)))
}

/// The catalog index of a DML target, by qualified name and then by bare name.
///
/// The bare fallback is not a guess: [`pgddl`][crate::pgddl] keys the catalog on bare names by
/// construction, and [`strip_schema`][crate::normalize::strip_schema] makes the query's own reference
/// bare before [`lower`][crate::lower] resolves it — so this resolves the target the same way the
/// rest of the pipeline will. Where `strip_schema` declines to strip (a system schema, or one bare
/// name reached through two qualifiers) `lower` then fails to resolve the target and the pair is
/// refused, so a mismatch here costs a refusal and never a proof. A qualified target is common enough
/// in practice to be worth resolving here rather than refusing.
fn find_target(cat: &Catalog, name: &str) -> Option<usize> {
    cat.find(name).or_else(|| cat.find(name.rsplit('.').next().unwrap_or(name)))
}

/// One side's projection: the value each column of the target ends up with.
///
/// Returned rather than wrapped in a query, because both blocks of [`two_goals`] use the *same*
/// projection — which is what makes their slot types agree without a cast anywhere.
fn project(cols: &[String], sets: &[(String, &Expr)], pred: Option<&Expr>) -> Vec<SelectItem> {
    cols
        .iter()
        .map(|c| {
            let old = Expr::Identifier(Ident::new(c.clone()));
            let expr = match sets.iter().find(|(set, _)| set == c) {
                None => old,
                // No `WHERE` means every row is updated, so the assigned expression is the value
                // unconditionally and the `CASE` would have an unreachable `ELSE`.
                Some((_, value)) => match pred {
                    None => (*value).clone(),
                    Some(p) => Expr::Case {
                        case_token: AttachedToken::empty(),
                        end_token: AttachedToken::empty(),
                        operand: None,
                        conditions: vec![CaseWhen {
                            condition: p.clone(),
                            result: (*value).clone(),
                        }],
                        else_result: Some(Box::new(old)),
                    },
                },
            };
            // Aliased even where the expression is the bare column, so the output names are the
            // catalog's and do not depend on how `lower` names an unaliased projection item.
            SelectItem::ExprWithAlias { expr, alias: Ident::new(c.clone()) }
        })
        .collect()
}

/// One side's two goals as a single query: the tagged `UNION ALL` the module docs describe.
///
/// Block 0 is the final table and block 1 the rows the statement matched. They share one
/// projection over one relation, so the blocks agree on arity and on slot types by construction and
/// neither needs a padding literal whose type the catalog might not be able to spell. Inside block
/// 1 every `CASE` takes its `THEN` arm — the block's own `WHERE` is the same predicate — so
/// keeping the `CASE` rather than the bare assigned value costs nothing and is what keeps the two
/// blocks textually identical bar the filter.
///
/// The skeleton is parsed from a constant for the reason [`select`] gives, and already carries the
/// two tag items, so nothing here hand-builds a numeric literal either.
fn two_goals(proj: Vec<SelectItem>, from: &TableWithJoins, pred: Option<&Expr>) -> Query {
    let mut parsed = Parser::parse_sql(
        &crate::DIALECT,
        "SELECT 0 AS q_goal FROM t UNION ALL SELECT 1 AS q_goal FROM t",
    )
    .expect("the skeleton is a constant");
    let Some(Statement::Query(mut q)) = parsed.pop() else {
        unreachable!("the skeleton is one query")
    };
    let SetExpr::SetOperation { left, right, .. } = &mut *q.body else {
        unreachable!("the skeleton is a union")
    };
    for (block, selection) in [(left, None), (right, pred.cloned())] {
        let SetExpr::Select(s) = &mut **block else { unreachable!("each block is a SELECT") };
        s.projection.extend(proj.iter().cloned());
        s.from = vec![from.clone()];
        s.selection = selection;
    }
    *q
}

/// The parts of an `INSERT` the reduction reads: the target's name, the qualifier a `RETURNING`
/// item may name a column of the target with, the column list under [`fold_ident`], and the source.
type InsertParts<'a> = (&'a ObjectName, String, Vec<String>, &'a Query);

/// Read an `INSERT` into its [`InsertParts`], refusing every shape the reduction does not model.
///
/// Unlike a `DELETE` or an `UPDATE`, whose target factor is carried across into the reduced query
/// where [`lower`][crate::lower] meets whatever else it holds, an `INSERT` is *replaced* by its
/// source and every field not read here is dropped. So this list has to be exhaustive over the
/// struct rather than over what the corpus happens to contain: a field silently ignored is a
/// clause silently deleted. Only `into`, `has_table_keyword` and the tokens are pure spelling.
fn insert_shape(i: &Insert) -> Result<InsertParts<'_>> {
    // Upsert is `T := f_S(T)`: which rows it writes is decided by which rows already match, so the
    // effect is not bag addition and the two sources are not the observable. MySQL's
    // `ON DUPLICATE KEY UPDATE` is the same shape under another name.
    if i.on.is_some() {
        return Err(unsupported("INSERT ... ON CONFLICT / ON DUPLICATE KEY"));
    }
    // Every one of these changes what is stored, or how much of it, in a way the source bag does
    // not say: `OR IGNORE`/`IGNORE`/`REPLACE` turn a constraint violation into a skip or an
    // overwrite, `OVERWRITE` and `PARTITION` replace rather than add, `SET` is a different source
    // syntax, and the Snowflake multi-table forms have several targets.
    if i.or.is_some() || i.ignore || i.replace_into || i.priority.is_some() {
        return Err(unsupported("INSERT OR / IGNORE / REPLACE / <priority>"));
    }
    if i.overwrite || i.partitioned.is_some() || !i.after_columns.is_empty() {
        return Err(unsupported("INSERT OVERWRITE / PARTITION"));
    }
    if !i.assignments.is_empty() {
        return Err(unsupported("INSERT ... SET"));
    }
    if i.multi_table_insert_type.is_some()
        || !i.multi_table_into_clauses.is_empty()
        || !i.multi_table_when_clauses.is_empty()
    {
        return Err(unsupported("multi-table INSERT"));
    }
    // T-SQL's `OUTPUT` is `RETURNING` with more options; `AS new (…)` is MySQL's row alias for the
    // `ON DUPLICATE KEY` clause, and `SETTINGS`/`FORMAT` are ClickHouse's.
    if i.output.is_some() {
        return Err(unsupported("INSERT ... OUTPUT"));
    }
    if i.insert_alias.is_some() {
        return Err(unsupported("INSERT ... AS <row alias>"));
    }
    if i.settings.is_some() || i.format_clause.is_some() {
        return Err(unsupported("INSERT ... SETTINGS / FORMAT"));
    }
    let TableObject::TableName(name) = &i.table else {
        return Err(unsupported("INSERT target is not a plain table"));
    };
    // `DEFAULT VALUES` stores one row of defaults. There is no source bag, so there is nothing for
    // the reduced query to be.
    let Some(source) = i.source.as_deref() else {
        return Err(unsupported("INSERT ... DEFAULT VALUES"));
    };
    // Without a list the source's columns are matched to the declared prefix positionally, and how
    // far along that prefix they reach is the source's arity — which for a `SELECT` source is not
    // known here, and which is exactly what decides the set the guard in [`insert_pair`] checks.
    // (Both sides being positional over one declared order is not the problem: the order cancels.)
    if i.columns.is_empty() {
        return Err(unsupported("INSERT without a column list"));
    }
    let mut cols: Vec<String> = Vec::new();
    for c in &i.columns {
        // Postgres does not allow a column of the insert list to be qualified, and a multi-part
        // name here would name a column of something other than the target.
        let [part] = c.0.as_slice() else {
            return Err(unsupported("qualified INSERT column"));
        };
        let Some(id) = part.as_ident() else {
            return Err(unsupported("INSERT column is not an identifier"));
        };
        let col = fold_ident(id);
        // Postgres rejects a repeated target column; the guard below reads the list as a set, so
        // it would not notice.
        if cols.contains(&col) {
            return Err(unsupported(format!("INSERT lists {col} twice")));
        }
        cols.push(col);
    }
    // An alias hides the table's own name, exactly as in [`target_qual`].
    let qual = match &i.table_alias {
        Some(a) => fold_ident(&a.alias),
        None => name.0.last().and_then(|p| p.as_ident()).map(fold_ident).unwrap_or_default(),
    };
    Ok((name, qual, cols, source))
}

/// Reduce a pair of `INSERT`s to the pair of queries computing the bags they add.
///
/// `INSERT INTO T (c1, …, cm) S` has effect `T := T ⊎ σ(S)`, where `σ` extends each row of the
/// source with a value for every column the list omits. Bag addition is cancellative, so the two
/// final tables agree iff `σ_A(S_A) = σ_B(S_B)` — and the reduced query is the source itself
/// provided two things hold.
///
/// The first is that the two column lists agree, which makes `σ` the same map on both sides. The
/// second is that `σ` is a **function of the row**, which is what
/// [`row_determined`][crate::catalog::Table::row_determined] records and what the guard below
/// reads. `nextval()` is a function of *position*: without the guard,
/// `INSERT INTO t (name) VALUES ('x'),('y')` and the same two rows written in the other order have
/// equal source bags and leave different tables, so the reduction would prove a pair that is not
/// equivalent. That is why the bit had to be added to the catalog before this arm could exist.
///
/// `RETURNING` needs no second goal here, unlike on an `UPDATE`. The returned bag is `π_L(σ(S))`,
/// which the source-bag goal already pins down when `L` is the same list on both sides — the same
/// argument that lets a `DELETE` drop the clause, and it is `σ`'s row-determinacy that carries it
/// over to an `INSERT`. So an identical list drops and a differing one is refused.
fn insert_pair(cat: &Catalog, a: &Insert, b: &Insert) -> Result<(Query, Query)> {
    let (na, qa, ca, sa) = insert_shape(a)?;
    let (nb, qb, cb, sb) = insert_shape(b)?;
    let (name, other) = (obj_name(na), obj_name(nb));
    if name.to_lowercase() != other.to_lowercase() {
        return Err(unsupported(format!("the pair's two statements target {name} and {other}")));
    }
    // Content *and* order: the source bag is a bag of tuples, so two lists naming the same columns
    // in two orders make `σ` two different maps.
    if ca != cb {
        return Err(unsupported("INSERT ... whose column lists differ"));
    }
    same_returning(a.returning.as_deref(), b.returning.as_deref(), &qa, &qb, "INSERT")?;

    let idx = find_target(cat, &name)
        .ok_or_else(|| schema(format!("no declared schema for INSERT target {name}")))?;
    let t = &cat.tables[idx];
    // The declared prefix, for the reason [`update_pair`] gives: a system column is not written by
    // an `INSERT` and would look to the guard like an omitted one.
    let declared = &t.cols[..t.n_declared];
    // Compared lowercased because `catalog::scan_ddl` lowercases a quoted declaration too, so a
    // `RETURNING`-faithful fold is not the identity the catalog is keyed on. A listed column the
    // catalog does not declare is refused rather than ignored: it is written by the statement and
    // invisible to the guard, which would then compute the omitted set wrongly.
    for c in &ca {
        if !declared.iter().any(|(d, _)| *d == c.to_lowercase()) {
            return Err(schema(format!("INSERT lists {c}, which {name} does not declare")));
        }
    }
    for (k, (col, _)) in declared.iter().enumerate() {
        if ca.iter().any(|c| c.to_lowercase() == *col) || t.row_determined[k] {
            continue;
        }
        return Err(unsupported(format!(
            "INSERT omits {col}, whose default is not a function of the row"
        )));
    }
    Ok((sa.clone(), sb.clone()))
}

/// A `Query` for `SELECT <projection> FROM <from> [WHERE <selection>]`.
///
/// The skeleton is parsed from a constant rather than written as a struct literal. `Query` and
/// `Select` have some forty fields between them, and all but the three set here are dialect
/// extensions this reduction has no opinion about — `PREWHERE`, `QUALIFY`, `CONNECT BY`, `CLUSTER BY`,
/// optimizer hints, `SELECT AS STRUCT`. Naming every one would mean editing this file on a sqlparser
/// bump to restate a value that is already "whatever the parser produces for a query not using it".
///
/// Note what this is *not*: rendering the reduced query as text and re-parsing it. Two of the
/// subtrees spliced in here — the predicate and the assigned expressions — are the reason the
/// reduction can be unsound at all, and a round trip through `Display` would put them back through a
/// parser that has [a precedence bug][crate::normalize] in the first place.
fn select(projection: Vec<SelectItem>, from: TableWithJoins, selection: Option<Expr>) -> Query {
    let mut parsed =
        Parser::parse_sql(&crate::DIALECT, "SELECT 1 FROM t").expect("the skeleton is a constant");
    let Some(Statement::Query(mut q)) = parsed.pop() else {
        unreachable!("the skeleton is one query")
    };
    let SetExpr::Select(select) = &mut *q.body else { unreachable!("the skeleton is a SELECT") };
    select.projection = projection;
    select.from = vec![from];
    select.selection = selection;
    *q
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalog::Table;

    /// A catalog with one table, every column an `INTEGER`, nullable, and row-determined.
    fn catalog(name: &str, cols: &[&str]) -> Catalog {
        volatile(name, cols, &[])
    }

    /// The same, with the named columns marked *not* row-determined — a `BIGSERIAL` or a
    /// `DEFAULT nextval(…)`, as far as [`insert_pair`]'s guard is concerned.
    fn volatile(name: &str, cols: &[&str], vol: &[&str]) -> Catalog {
        Catalog {
            tables: vec![Table {
                name: name.to_string(),
                cols: cols.iter().map(|c| (c.to_string(), "INTEGER".to_string())).collect(),
                nullable: vec![true; cols.len()],
                row_determined: cols.iter().map(|c| !vol.contains(c)).collect(),
                keys: Vec::new(),
                n_declared: cols.len(),
            }],
        }
    }

    /// Reduce a `;`-separated input and render whatever queries come out.
    fn reduced(cat: &Catalog, sql: &str) -> std::result::Result<Vec<String>, String> {
        let mut st = Parser::parse_sql(&crate::DIALECT, sql).expect("parses");
        reduce(cat, &mut st).map_err(|e| e.to_string())?;
        Ok(st
            .iter()
            .filter_map(|s| match s {
                Statement::Query(q) => Some(q.to_string()),
                _ => None,
            })
            .collect())
    }

    fn err(cat: &Catalog, sql: &str) -> String {
        reduced(cat, sql).expect_err("should refuse")
    }

    #[test]
    fn delete_becomes_the_deleted_bag() {
        let cat = catalog("t", &["a", "b"]);
        assert_eq!(
            reduced(&cat, "DELETE FROM t WHERE a = 1; DELETE FROM t;").unwrap(),
            ["SELECT * FROM t WHERE a = 1", "SELECT * FROM t"]
        );
    }

    /// The alias goes across with the relation, so a predicate qualified by it still resolves.
    #[test]
    fn delete_keeps_the_target_alias() {
        let cat = catalog("t", &["a"]);
        assert_eq!(
            reduced(&cat, "DELETE FROM t AS x WHERE x.a = 1; DELETE FROM t AS y;").unwrap(),
            ["SELECT * FROM t AS x WHERE x.a = 1", "SELECT * FROM t AS y"]
        );
    }

    #[test]
    fn delete_carries_its_with_clause() {
        let cat = catalog("t", &["a"]);
        let out = reduced(
            &cat,
            "WITH c AS (SELECT a FROM u) DELETE FROM t WHERE a IN (SELECT a FROM c);
             DELETE FROM t WHERE a IN (SELECT a FROM u);",
        )
        .unwrap();
        assert!(out[0].starts_with("WITH c AS (SELECT a FROM u) SELECT * FROM t"), "{}", out[0]);
    }

    #[test]
    fn identical_returning_on_a_delete_is_dropped() {
        let cat = catalog("t", &["a"]);
        assert_eq!(
            reduced(&cat, "DELETE FROM t RETURNING a; DELETE FROM t WHERE a = a RETURNING a;")
                .unwrap(),
            ["SELECT * FROM t", "SELECT * FROM t WHERE a = a"]
        );
    }

    /// The two lists are the same projection written two ways, so the clause still drops.
    #[test]
    fn a_qualified_returning_list_folds_to_the_unqualified_one() {
        let cat = catalog("t", &["a", "b"]);
        assert_eq!(
            reduced(&cat, "DELETE FROM t AS u RETURNING u.a, b; DELETE FROM t RETURNING a, t.b;")
                .unwrap(),
            ["SELECT * FROM t AS u", "SELECT * FROM t"]
        );
        assert_eq!(
            reduced(&cat, "DELETE FROM t AS u RETURNING u.*; DELETE FROM t RETURNING *;").unwrap(),
            ["SELECT * FROM t AS u", "SELECT * FROM t"]
        );
    }

    /// A qualifier that does not name the target is not folded away, so the item stays structural.
    #[test]
    fn a_foreign_qualifier_is_not_folded() {
        let cat = catalog("t", &["a", "b"]);
        assert!(err(&cat, "DELETE FROM t AS u RETURNING v.a; DELETE FROM t RETURNING a;")
            .contains("RETURNING that differs"));
    }

    #[test]
    fn one_sided_or_differing_returning_is_refused() {
        let cat = catalog("t", &["a", "b"]);
        for (sql, want) in [
            ("DELETE FROM t RETURNING a; DELETE FROM t;", "DELETE ... RETURNING on one side only"),
            (
                "UPDATE t SET a = 1 RETURNING a; UPDATE t SET a = 1;",
                "UPDATE ... RETURNING on one side only",
            ),
            (
                "DELETE FROM t RETURNING a; DELETE FROM t RETURNING b;",
                "DELETE ... RETURNING that differs between the two queries",
            ),
            (
                "UPDATE t SET a = 1 RETURNING a; UPDATE t SET a = 1 RETURNING b;",
                "UPDATE ... RETURNING that differs between the two queries",
            ),
            // Refused structurally: the check never expands `*` against the catalog, so a wildcard
            // matches no explicit list. Here `a, b` *is* `*` in declared order, so the pair is in
            // fact equivalent and this is incompleteness, not unsoundness. Expanding it grows the
            // provable set and so needs the sqleq-fuzz cross-check before it ships.
            (
                "DELETE FROM t RETURNING *; DELETE FROM t RETURNING a, b;",
                "DELETE ... RETURNING that differs between the two queries",
            ),
        ] {
            assert!(err(&cat, sql).contains(want), "{sql}");
        }
    }

    #[test]
    fn join_delete_is_refused() {
        let cat = catalog("t", &["a"]);
        assert!(err(&cat, "DELETE FROM t USING u WHERE t.a = u.a; DELETE FROM t;")
            .contains("join-delete"));
    }

    #[test]
    fn a_delete_paired_with_a_select_is_refused() {
        let cat = catalog("t", &["a"]);
        assert_eq!(
            err(&cat, "DELETE FROM t WHERE a = 1; SELECT * FROM t WHERE a = 1;"),
            "unsupported: DELETE paired with non-DELETE"
        );
    }

    #[test]
    fn two_statements_on_two_tables_are_refused() {
        let cat = catalog("t", &["a"]);
        assert!(err(&cat, "DELETE FROM t; DELETE FROM u;").contains("target t and u"));
    }

    /// The whole point of the `UPDATE` correction: `b` is projected although neither side sets it, so
    /// the two projections are the different bags the two statements actually leave behind.
    #[test]
    fn update_projects_every_column_of_the_target() {
        let cat = catalog("t", &["a", "b"]);
        assert_eq!(
            reduced(&cat, "UPDATE t SET a = 3 - a; UPDATE t SET a = a;").unwrap(),
            ["SELECT 3 - a AS a, b AS b FROM t", "SELECT a AS a, b AS b FROM t"]
        );
    }

    #[test]
    fn the_predicate_becomes_a_case_over_the_old_value() {
        let cat = catalog("t", &["a", "b"]);
        assert_eq!(
            reduced(&cat, "UPDATE t SET a = 1 WHERE b = 2; UPDATE t SET b = 1;").unwrap(),
            [
                "SELECT CASE WHEN b = 2 THEN 1 ELSE a END AS a, b AS b FROM t",
                "SELECT a AS a, 1 AS b FROM t"
            ]
        );
    }

    #[test]
    fn a_qualified_target_resolves_on_its_bare_name() {
        let cat = catalog("t", &["a"]);
        assert_eq!(
            reduced(&cat, "UPDATE part_7.t SET a = 1; UPDATE part_7.t SET a = 1;").unwrap(),
            ["SELECT 1 AS a FROM part_7.t", "SELECT 1 AS a FROM part_7.t"]
        );
    }

    /// An identical `RETURNING` list is not droppable on an `UPDATE`; it becomes a second block.
    #[test]
    fn update_returning_becomes_a_second_goal() {
        let cat = catalog("t", &["a", "b"]);
        assert_eq!(
            reduced(
                &cat,
                "UPDATE t SET a = 1 WHERE b = 2 RETURNING a; UPDATE t SET a = 1 RETURNING a;"
            )
            .unwrap(),
            [
                "SELECT 0 AS q_goal, CASE WHEN b = 2 THEN 1 ELSE a END AS a, b AS b FROM t \
                 UNION ALL SELECT 1 AS q_goal, CASE WHEN b = 2 THEN 1 ELSE a END AS a, b AS b \
                 FROM t WHERE b = 2",
                "SELECT 0 AS q_goal, 1 AS a, b AS b FROM t \
                 UNION ALL SELECT 1 AS q_goal, 1 AS a, b AS b FROM t",
            ]
        );
    }

    /// The asymmetry the module docs open with, as a test: two `UPDATE`s that leave the table
    /// untouched and return different bags must not reduce to the same query.
    #[test]
    fn a_no_op_update_is_separated_by_its_second_goal() {
        let cat = catalog("t", &["a"]);
        let out = reduced(
            &cat,
            "UPDATE t SET a = a WHERE a = 1 RETURNING a; UPDATE t SET a = a WHERE a = 2 RETURNING a;",
        )
        .unwrap();
        assert_ne!(out[0], out[1]);
        assert!(out[0].ends_with("FROM t WHERE a = 1"), "{}", out[0]);
        assert!(out[1].ends_with("FROM t WHERE a = 2"), "{}", out[1]);
        // Without the clause the very same pair collapses to one query twice over -- which is the
        // false proof the second goal exists to stop.
        let bare =
            reduced(&cat, "UPDATE t SET a = a WHERE a = 1; UPDATE t SET a = a WHERE a = 2;")
                .unwrap();
        assert_eq!(bare[0], "SELECT CASE WHEN a = 1 THEN a ELSE a END AS a FROM t");
    }

    /// `RETURNING` items that are not columns cost nothing: the list is compared, never evaluated.
    #[test]
    fn an_opaque_returning_item_is_compared_structurally() {
        let cat = catalog("t", &["a"]);
        assert!(reduced(
            &cat,
            "UPDATE t SET a = 1 RETURNING host(a); UPDATE t SET a = 1 RETURNING host(a);"
        )
        .unwrap()[0]
            .contains("q_goal"));
        assert!(err(&cat, "UPDATE t SET a = 1 RETURNING $4; UPDATE t SET a = 1 RETURNING a;")
            .contains("RETURNING that differs"));
    }

    #[test]
    fn update_of_an_undeclared_table_is_refused() {
        let cat = catalog("t", &["a"]);
        assert!(err(&cat, "UPDATE u SET a = 1; UPDATE u SET a = 1;")
            .contains("no declared schema for UPDATE target u"));
    }

    #[test]
    fn update_of_an_undeclared_column_is_refused() {
        let cat = catalog("t", &["a"]);
        assert!(err(&cat, "UPDATE t SET z = 1; UPDATE t SET a = 1;")
            .contains("UPDATE sets z, which t does not declare"));
    }

    #[test]
    fn row_assignment_and_join_update_are_refused() {
        let cat = catalog("t", &["a", "b"]);
        assert!(err(&cat, "UPDATE t SET (a, b) = (SELECT 1, 2); UPDATE t SET a = 1;")
            .contains("row assignment"));
        assert!(err(&cat, "UPDATE t SET a = u.a FROM u; UPDATE t SET a = 1;")
            .contains("join-update"));
    }

    #[test]
    fn an_update_paired_with_a_non_update_is_refused() {
        let cat = catalog("t", &["a"]);
        assert_eq!(
            err(&cat, "UPDATE t SET a = 1; SELECT a FROM t;"),
            "unsupported: UPDATE paired with non-UPDATE"
        );
    }

    #[test]
    fn an_insert_becomes_its_source_bag() {
        let cat = catalog("t", &["a", "b"]);
        assert_eq!(
            reduced(
                &cat,
                "INSERT INTO t (a, b) VALUES (1, 2), (3, 4);
                 INSERT INTO t (a, b) SELECT x, y FROM u;",
            )
            .unwrap(),
            ["VALUES (1, 2), (3, 4)", "SELECT x, y FROM u"]
        );
    }

    /// Every column the list omits takes its default; where that default is a function of the row
    /// the source bag still pins the stored bag, so there is nothing more to compare.
    #[test]
    fn an_insert_may_omit_a_row_determined_column() {
        let cat = catalog("t", &["a", "b"]);
        assert_eq!(
            reduced(&cat, "INSERT INTO t (a) VALUES (1); INSERT INTO t (a) VALUES (1);").unwrap(),
            ["VALUES (1)", "VALUES (1)"]
        );
    }

    /// The false proof the guard exists to stop. Both sides insert the same *bag* of names and
    /// leave different tables, because `nextval()` is a function of position and not of the row.
    #[test]
    fn an_insert_omitting_a_volatile_column_is_refused() {
        let cat = volatile("t", &["id", "name"], &["id"]);
        assert_eq!(
            err(
                &cat,
                "INSERT INTO t (name) VALUES ('x'), ('y');
                 INSERT INTO t (name) VALUES ('y'), ('x');",
            ),
            "unsupported: INSERT omits id, whose default is not a function of the row"
        );
        // Naming the column puts its value back in the source bag, so the pair reduces again.
        assert!(reduced(
            &cat,
            "INSERT INTO t (id, name) VALUES (1, 'x'); INSERT INTO t (id, name) VALUES (1, 'x');",
        )
        .is_ok());
    }

    /// The source is a bag of *tuples*, so two lists over the same columns in two orders are two
    /// different maps from it to the stored rows.
    #[test]
    fn insert_column_lists_that_differ_in_content_or_order_are_refused() {
        let cat = catalog("t", &["a", "b"]);
        for sql in [
            "INSERT INTO t (a, b) VALUES (1, 2); INSERT INTO t (b, a) VALUES (2, 1);",
            "INSERT INTO t (a, b) VALUES (1, 2); INSERT INTO t (a) VALUES (1);",
        ] {
            assert!(err(&cat, sql).contains("column lists differ"), "{sql}");
        }
        // Case and quoting fold the way Postgres folds them, so these two are one list.
        assert!(reduced(
            &cat,
            r#"INSERT INTO t (A, b) VALUES (1, 2); INSERT INTO t ("a", B) VALUES (1, 2);"#,
        )
        .is_ok());
    }

    /// Unlike an `UPDATE`, an `INSERT` needs no second goal: the returned bag is a projection of
    /// the stored bag, which the source-bag goal already pins.
    #[test]
    fn identical_returning_on_an_insert_is_dropped() {
        let cat = catalog("t", &["a", "b"]);
        assert_eq!(
            reduced(
                &cat,
                "INSERT INTO t (a, b) VALUES (1, 2) RETURNING a, t.b;
                 INSERT INTO t AS z (a, b) VALUES (1, 2) RETURNING z.a, b;",
            )
            .unwrap(),
            ["VALUES (1, 2)", "VALUES (1, 2)"]
        );
    }

    #[test]
    fn one_sided_or_differing_returning_on_an_insert_is_refused() {
        let cat = catalog("t", &["a", "b"]);
        assert_eq!(
            err(&cat, "INSERT INTO t (a) VALUES (1) RETURNING a; INSERT INTO t (a) VALUES (1);"),
            "unsupported: INSERT ... RETURNING on one side only"
        );
        assert!(err(
            &cat,
            "INSERT INTO t (a) VALUES (1) RETURNING a; INSERT INTO t (a) VALUES (1) RETURNING b;",
        )
        .contains("RETURNING that differs"));
    }

    /// Upsert is `T := f_S(T)`, not bag addition — the one bucket that holds most of the corpus.
    #[test]
    fn on_conflict_is_refused() {
        let cat = catalog("t", &["a", "b"]);
        assert!(err(
            &cat,
            "INSERT INTO t (a) VALUES (1) ON CONFLICT (a) DO NOTHING;
             INSERT INTO t (a) VALUES (1);",
        )
        .contains("ON CONFLICT"));
    }

    #[test]
    fn an_insert_without_a_column_list_or_without_a_source_is_refused() {
        let cat = catalog("t", &["a", "b"]);
        assert_eq!(
            err(&cat, "INSERT INTO t VALUES (1, 2); INSERT INTO t VALUES (1, 2);"),
            "unsupported: INSERT without a column list"
        );
        assert_eq!(
            err(&cat, "INSERT INTO t DEFAULT VALUES; INSERT INTO t DEFAULT VALUES;"),
            "unsupported: INSERT ... DEFAULT VALUES"
        );
    }

    /// A column the catalog does not have is written by the statement and invisible to the guard,
    /// which would then read the omitted set off a declaration that does not describe this table.
    #[test]
    fn an_insert_of_an_undeclared_column_or_into_an_undeclared_table_is_refused() {
        let cat = catalog("t", &["a"]);
        assert!(err(&cat, "INSERT INTO t (z) VALUES (1); INSERT INTO t (z) VALUES (1);")
            .contains("INSERT lists z, which t does not declare"));
        assert!(err(&cat, "INSERT INTO u (a) VALUES (1); INSERT INTO u (a) VALUES (1);")
            .contains("no declared schema for INSERT target u"));
    }

    #[test]
    fn inserts_into_two_tables_and_an_insert_paired_with_a_non_insert_are_refused() {
        let cat = catalog("t", &["a"]);
        assert!(err(&cat, "INSERT INTO t (a) VALUES (1); INSERT INTO u (a) VALUES (1);")
            .contains("target t and u"));
        assert_eq!(
            err(&cat, "INSERT INTO t (a) VALUES (1); SELECT a FROM t;"),
            "unsupported: INSERT paired with non-INSERT"
        );
    }

    /// The bindings move onto the reduced query, as they do for the other two kinds — but here the
    /// reduced query is one the input already contained, so it may have bindings of its own.
    #[test]
    fn an_insert_carries_its_with_clause_and_refuses_two_of_them() {
        let cat = catalog("t", &["a"]);
        let out = reduced(
            &cat,
            "WITH c AS (SELECT a FROM u) INSERT INTO t (a) SELECT a FROM c;
             INSERT INTO t (a) SELECT a FROM u;",
        )
        .unwrap();
        assert_eq!(out[0], "WITH c AS (SELECT a FROM u) SELECT a FROM c");
        assert_eq!(
            err(
                &cat,
                "WITH c AS (SELECT a FROM u) INSERT INTO t (a) WITH d AS (SELECT a FROM c)
                     SELECT a FROM d;
                 INSERT INTO t (a) SELECT a FROM u;",
            ),
            "unsupported: WITH on both the INSERT and its source"
        );
    }
}
