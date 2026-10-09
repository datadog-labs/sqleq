// Unless explicitly stated otherwise all files in this repository are licensed under the
// Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/).
// Copyright 2026-Present Datadog, Inc.

//! `DELETE`, `UPDATE` and `INSERT` reduced to the `SELECT` that computes their effect.
//!
//! The prover's IR has relations and no statements: nothing in it mutates a table, so a pair of
//! `DELETE`s, `UPDATE`s or `INSERT`s cannot be handed to it as written. What *can* be handed to it
//! is a query computing the thing the two statements have to agree on, which turns "are these two
//! statements equivalent" into "are these two queries bag-equivalent" — the question the prover
//! already answers. Three shapes occur in practice and all three are handled: both sides a bare
//! `DELETE`/`UPDATE`, one side wrapped in a `WITH`, and the two mixed across the pair.
//!
//! Like the rewrites in [`normalize`][crate::normalize] this is a rewrite whose verdict is reported
//! for the *original* pair, so an unsound reduction does not fail, it lies. Each reduction carries
//! its equivalence argument below and every precondition of that argument is a guard in the code.
//!
//! One precondition is shared by all three: the table stores what the statement writes, and the
//! statement writes nothing else. A trigger breaks it (a `BEFORE` trigger rewrites or skips the row,
//! an `AFTER` one writes other tables), and so does a rule (`DO INSTEAD` is another statement), so a
//! statement on a table either names is refused ([`no_trigger`]).
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
//! ## `DELETE ... USING` and `UPDATE ... FROM`
//!
//! ```text
//! DELETE FROM T USING U WHERE P       ~>   SELECT * FROM T WHERE EXISTS (SELECT 1 FROM U WHERE P)
//! UPDATE T SET c = e FROM F WHERE P   ~>   the projection below, with P read as that EXISTS
//! ```
//!
//! Postgres deletes or updates a target row once if some row of the join makes `P` TRUE, so the
//! rows affected are exactly the ones the `EXISTS` keeps, and the arguments here go through
//! unchanged. What the join adds is a second source of values: when several rows match, an `UPDATE`
//! takes its new values from an unspecified one of them, and a `RETURNING` item that reads `U` or `F`
//! reports an unspecified one too. So the reduction applies only where nothing the statement assigns
//! or returns may read the joined relations ([`check_using`], [`from_is_a_filter`]). An `UPDATE`
//! whose `SET` reads `F` is a keyed join-update, and stays refused.
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
//! ## `INSERT`
//!
//! ```text
//! INSERT INTO T (c1, …, cm) S   ~>   S
//! ```
//!
//! The effect is `T := T ⊎ σ(S)`, where `σ` fills in every column the list omits, and bag addition
//! is cancellative, so two `INSERT`s into one table leave equal tables iff `σ(S₁) = σ(S₂)`. With
//! one column list on both sides that is `S₁ = S₂` — provided `σ` is a function of the row, which a
//! `nextval()` default is not: it numbers rows by position. So an `INSERT` that omits a column
//! whose default is not row-determined is refused, as are the shapes whose effect is not bag
//! addition (`ON CONFLICT`, `DEFAULT VALUES`, …). The argument and its guards are on
//! [`insert_pair`]. `RETURNING` drops when both lists are the same, for the reason it drops on a
//! `DELETE`.
//!
//! ## The cast an assignment applies
//!
//! Postgres stores a value in a column of another type through the assignment cast to the column's
//! type: `SET s = n`, with `s` text and `n` numeric, stores `n::text`, and so does an
//! `INSERT INTO t (s)` whose source yields `n`. The two reductions above hand the provers `n`
//! itself, which they read as its class under `=` ([`equality`][crate::equality]). That is the
//! stored value only where the cast is a function of the class, and for a type whose `=` is not
//! identity it need not be: `2.0 = 2.00`, while `'2.0'` and `'2.00'` are two strings. So
//!
//! ```text
//! A: UPDATE t SET s = n WHERE n = m      B: UPDATE t SET s = m WHERE n = m
//! ```
//!
//! reduce to two projections a prover proves equal, and on `t = {(NULL, 2.0, 2.00)}` A stores
//! `'2.0'` where B stores `'2.00'`.
//!
//! So a value an `UPDATE` or an `INSERT` stores is read as the cast that stores it, the way
//! [`equality`][crate::equality] reads an explicit cast ([`stores_by_value`][crate::equality::stores_by_value]):
//! a cast to a number type, to a boolean or to another type whose `=` is not identity converts by
//! value -- a `numeric(10,2)` column rounds `2.0` and `2.00` to the same `2.00` -- and a cast to
//! text, `json` or any other type is not known to. Where it is not, [`refuse_observed_stores`]
//! reads the stored value through `q_exact_<type>` if its spelling fixes it, as `SET s = 2.0`'s
//! does, and refuses the pair otherwise. One modifier does not convert by value: an interval
//! column's. `interval day` keeps only the days of a value stored in it, so `'1 day'` stays and
//! `'24 hours'` becomes `0`, and `interval(0)` rounds the seconds away from zero, so
//! `'1 day -00:00:00.5'` and `'23:59:59.5'` are stored as two intervals `=` tells apart. Hence
//! [`declared_types`][crate::catalog::Table::declared_types], which keeps the modifier the prover
//! type drops.
//!
//! The check reads the lowered plans, because only there is the stored value's type known. Unlike
//! [`equality::refuse_observed`][crate::equality::refuse_observed], it is never lifted where the two
//! sides lower to one plan. One plan stores values of one class, but not always the same member of
//! it: a `DISTINCT` keeps whichever of `2.0` and `2.00` reaches it first, and the order they reach
//! it in can be a subquery's `ORDER BY` that the lowering drops. The reads decline their exception
//! for that reason only where a subquery's `ORDER BY` was dropped (`crate::emit`); this check
//! declines it everywhere, which refuses more and does not depend on that test.
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
//! A `DELETE` is reduced only against another `DELETE`, an `UPDATE` only against another `UPDATE`,
//! and an `INSERT` only against another `INSERT` into the same table.
//! The preprocessor reduces a `DELETE` on its own and then compares it with whatever the other side
//! is, so a `DELETE` paired with a plain `SELECT` becomes "deleted bag vs. query result" and a
//! statement that mutates a table can be reported equivalent to one that reads it. The `UPDATE`
//! reduction is *jointly* defined anyway (both sides project the same column list), so it needs the
//! pair regardless.
//!
//! ## Where this runs
//!
//! Between [`fix_precedence`][crate::normalize::fix_precedence] and every normalization. After the
//! precedence guard, because the reduction copies the `WHERE` predicate into one `CASE` per column and
//! the tree it copies has to be the one the SQL means; before the normalizations, because then no other pass
//! needs to know that DML exists — everything downstream of here sees two queries.

use std::ops::ControlFlow;

use sqlparser::ast::helpers::attached_token::AttachedToken;
use sqlparser::ast::{
    visit_expressions, CaseWhen, CastKind, Delete, Expr, FromTable, FunctionArg, FunctionArgExpr,
    FunctionArguments, Ident, Insert, ObjectName, Query, SelectItem, SelectItemQualifiedWildcardKind,
    SetExpr, Statement, TableFactor, TableObject, TableWithJoins, Update, UpdateTableFromKind,
    WildcardAdditionalOptions,
};
use sqlparser::parser::Parser;

use serde_json::Value;

use crate::catalog::{obj_name, Catalog, Table};
use crate::error::{schema, unsupported, Result};
use crate::types::coarse_class;

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
///
/// Returns, for each of the pair's two queries in order, where a reduced `UPDATE` or `INSERT`
/// stores its outputs, for [`refuse_observed_stores`] to read once they are lowered. Empty for any
/// other pair.
pub fn reduce(cat: &Catalog, statements: &mut [Statement]) -> Result<[Stores; 2]> {
    let deletes: Vec<usize> =
        (0..statements.len()).filter(|&i| as_delete(&statements[i]).is_some()).collect();
    let updates: Vec<usize> =
        (0..statements.len()).filter(|&i| as_update(&statements[i]).is_some()).collect();
    let inserts: Vec<usize> =
        (0..statements.len()).filter(|&i| as_insert(&statements[i]).is_some()).collect();
    for &i in deletes.iter().chain(&updates).chain(&inserts) {
        no_default_keyword(&statements[i])?;
    }
    // A `DELETE` stores nothing. Two kinds reduced at once leave four queries, which
    // `collect_queries` refuses.
    let mut stores = [Stores::default(), Stores::default()];

    match deletes.len() {
        0 => {}
        2 => {
            let (i, j) = (deletes[0], deletes[1]);
            let (a, b) = (delete_at(statements, i), delete_at(statements, j));
            let (da, db) = (delete_target(a)?, delete_target(b)?);
            target_not_shadowed(&statements[i], da)?;
            target_not_shadowed(&statements[j], db)?;
            let (qa, qb) = (target_qual(da)?, target_qual(db)?);
            check_using(cat, a, da, &qa)?;
            check_using(cat, b, db, &qb)?;
            same_returning(a.returning.as_deref(), b.returning.as_deref(), &qa, &qb, "DELETE")?;
            same_target(da, db)?;
            no_trigger(cat, &obj_name(target_name(da)?))?;
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
            target_not_shadowed(&statements[i], &update_at(statements, i).table)?;
            target_not_shadowed(&statements[j], &update_at(statements, j).table)?;
            let (qa, qb, sa) = update_pair(cat, update_at(statements, i), update_at(statements, j))?;
            install(&mut statements[i], qa)?;
            install(&mut statements[j], qb)?;
            stores = sa;
        }
        _ => return Err(unsupported("UPDATE paired with non-UPDATE")),
    }

    match inserts.len() {
        0 => {}
        2 => {
            let (i, j) = (inserts[0], inserts[1]);
            let (qa, qb, sa) = insert_pair(cat, insert_at(statements, i), insert_at(statements, j))?;
            install(&mut statements[i], qa)?;
            install(&mut statements[j], qb)?;
            stores = sa;
        }
        _ => return Err(unsupported("INSERT paired with non-INSERT")),
    }
    Ok(stores)
}

/// Refuse a DML statement that uses the keyword `DEFAULT` as a value.
///
/// In `UPDATE t SET a = DEFAULT`, and in an `INSERT`'s `VALUES` row, `DEFAULT` stands for the
/// column's default. sqlparser gives it there as a plain unquoted identifier, and lowering would
/// resolve that as a column like any other: refused when the table has none of that name, but over a
/// table with a column `"default"`, `SET a = DEFAULT` would lower like `SET a = "default"`. The
/// reductions do not model a default, so the keyword is refused wherever it appears. `DEFAULT` is
/// reserved in Postgres, so an unquoted one is never a column reference (the quoted `"default"`, and
/// the qualified `t.default`, are), and refusing it costs no statement that reads a column.
fn no_default_keyword(st: &Statement) -> Result<()> {
    let found = visit_expressions(st, |e| match e {
        Expr::Identifier(id) if id.quote_style.is_none() && id.value.eq_ignore_ascii_case("default") => {
            ControlFlow::Break(())
        }
        _ => ControlFlow::Continue(()),
    });
    if found.is_break() {
        return Err(unsupported("DEFAULT as a value (a column default is not modelled)"));
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

/// Refuse a `WITH` binding that has the DML target's name.
///
/// The target of a `DELETE` or `UPDATE` always names the table, never a binding of the statement's
/// own `WITH`, while every other mention of that name in the statement does mean the binding. The
/// reduced query cannot keep the two apart: the target becomes an ordinary `FROM` item, and
/// [`inline_ctes`][crate::normalize::inline_ctes] then replaces it with the binding. So
/// `WITH t AS (SELECT * FROM t WHERE a = 1) DELETE FROM t`, which empties `t`, would lower like
/// `DELETE FROM t WHERE a = 1`. Compared on the bare name, because a schema qualifier on the target
/// is stripped later and would not keep the two apart either.
fn target_not_shadowed(st: &Statement, target: &TableWithJoins) -> Result<()> {
    let Statement::Query(wrapper) = st else { return Ok(()) };
    let Some(with) = &wrapper.with else { return Ok(()) };
    let Some(bare) = target_name(target)?.0.last().and_then(|p| p.as_ident()) else {
        return Ok(());
    };
    let bare = fold_ident(bare);
    if with.cte_tables.iter().any(|cte| fold_ident(&cte.alias.name) == bare) {
        return Err(unsupported("WITH binding named like the DML target"));
    }
    Ok(())
}

/// `DELETE FROM T WHERE P` -> `SELECT * FROM T WHERE P`, and `DELETE FROM T USING U WHERE P` ->
/// `SELECT * FROM T WHERE EXISTS (SELECT 1 FROM U WHERE P)`. See the module docs for the argument,
/// and [`check_using`] for what the second needs besides.
fn delete_to_select(d: &Delete) -> Result<Query> {
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
    let selection = match &d.using {
        None => d.selection.clone(),
        Some(using) => Some(exists(using.clone(), d.selection.clone())),
    };
    Ok(select(vec![SelectItem::Wildcard(WildcardAdditionalOptions::default())], target.clone(), selection))
}

/// The guards a `DELETE ... USING` needs on top of a plain `DELETE`'s.
///
/// Postgres deletes a target row once if some combination of `USING` rows makes the predicate TRUE,
/// which is exactly the rows the `EXISTS` of [`delete_to_select`] keeps: the deleted bag is still a
/// bag of the target's rows, and the plain argument goes through. Two things it does not settle:
///
/// * A `USING` relation named like the target. Inside the `EXISTS` its name would shadow the
///   target's, and a reference to the deleted row would read the joined one. (Postgres rejects the
///   statement; refusing keeps the reduction from depending on that.)
/// * `RETURNING`. Dropping it is sound only when the deleted bag determines what it returns, and an
///   item that reads a `USING` relation is evaluated against whichever matching row the join
///   produced. So only items that read the target alone are accepted: `q.*` and `q.c` with `q` the
///   target, a closed expression, and a bare name the catalog declares for the target and for no
///   `USING` table. A bare `*` expands to the `USING` relations' columns too.
fn check_using(cat: &Catalog, d: &Delete, target: &TableWithJoins, qual: &str) -> Result<()> {
    let Some(using) = &d.using else { return Ok(()) };
    let mut factors = Vec::new();
    for twj in using {
        using_factors(&twj.relation, &mut factors);
        twj.joins.iter().for_each(|j| using_factors(&j.relation, &mut factors));
    }
    // The columns a bare name in `RETURNING` could reach in the `USING` relations, or `None` when
    // some relation's columns are not known.
    let mut using_cols: Option<Vec<String>> = Some(Vec::new());
    for tf in factors {
        if factor_name(tf).as_deref() == Some(qual) {
            return Err(unsupported("DELETE ... USING a relation named like the target"));
        }
        let cols = match tf {
            TableFactor::Table { name, alias, args: None, .. }
                if alias.as_ref().is_none_or(|a| a.columns.is_empty()) =>
            {
                find_target(cat, &obj_name(name)).map(|i| cat.tables[i].cols.iter().map(|(c, _)| c.clone()))
            }
            _ => None,
        };
        match (&mut using_cols, cols) {
            (Some(all), Some(c)) => all.extend(c),
            _ => using_cols = None,
        }
    }
    let Some(items) = &d.returning else { return Ok(()) };
    let target_cols: Vec<String> = find_target(cat, &obj_name(target_name(target)?))
        .map(|i| cat.tables[i].cols.iter().map(|(c, _)| c.clone()).collect())
        .unwrap_or_default();
    let names_target = |q: &Ident| fold_ident(q) == qual;
    for item in items {
        let reads_target_only = match item {
            SelectItem::QualifiedWildcard(SelectItemQualifiedWildcardKind::ObjectName(n), o) => {
                *o == WildcardAdditionalOptions::default()
                    && matches!(n.0.as_slice(), [p] if p.as_ident().is_some_and(names_target))
            }
            SelectItem::UnnamedExpr(Expr::CompoundIdentifier(parts)) => matches!(parts.as_slice(), [q, _] if names_target(q)),
            SelectItem::UnnamedExpr(Expr::Identifier(c)) => {
                let c = fold_ident(c);
                target_cols.contains(&c) && using_cols.as_ref().is_some_and(|u| !u.contains(&c))
            }
            SelectItem::UnnamedExpr(e) | SelectItem::ExprWithAlias { expr: e, .. } => crate::lower::is_closed(e),
            _ => false,
        };
        if !reads_target_only {
            return Err(unsupported("DELETE ... USING with a RETURNING item that may read a USING relation"));
        }
    }
    Ok(())
}

/// The relations a `USING` item brings into scope, looking through parenthesised joins.
fn using_factors<'a>(tf: &'a TableFactor, out: &mut Vec<&'a TableFactor>) {
    match tf {
        TableFactor::NestedJoin { table_with_joins, alias: None } => {
            using_factors(&table_with_joins.relation, out);
            table_with_joins.joins.iter().for_each(|j| using_factors(&j.relation, out));
        }
        other => out.push(other),
    }
}

/// The name a relation is referred to by: its alias, or a table's own bare name.
fn factor_name(tf: &TableFactor) -> Option<String> {
    let alias = match tf {
        TableFactor::Table { alias, .. }
        | TableFactor::Derived { alias, .. }
        | TableFactor::Function { alias, .. }
        | TableFactor::UNNEST { alias, .. }
        | TableFactor::NestedJoin { alias, .. } => alias.as_ref(),
        _ => None,
    };
    match (alias, tf) {
        (Some(a), _) => Some(fold_ident(&a.name)),
        (None, TableFactor::Table { name, .. }) => name.0.last().and_then(|p| p.as_ident()).map(fold_ident),
        _ => None,
    }
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
/// not. The fold is ASCII-only, as Postgres's is under a multibyte server encoding: an unquoted `É`
/// stays `É`.
///
/// This is the language's rule and not a normalisation of convenience, because it decides whether
/// two `RETURNING` lists are the same projection, and which column or relation a name in a query
/// reads — folding too much is a false proof, folding too little is a refusal. The catalog stores a
/// column's name in this form, and name resolution in `lower` looks names up in it.
pub(crate) fn fold_ident(id: &Ident) -> String {
    match id.quote_style {
        Some(_) => id.value.clone(),
        None => id.value.to_ascii_lowercase(),
    }
}

/// An identifier that [`fold_ident`] reads back as `name`: unquoted where that already folds to it,
/// quoted otherwise.
fn spelled(name: &str) -> Ident {
    if name.to_ascii_lowercase() == name {
        Ident::new(name)
    } else {
        Ident::with_quote('"', name)
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
    /// `*`, or `q.*` where `q` names the target. Both expand to the target's declared columns, in
    /// declared order: the target is the only relation in scope, and under a `USING`/`FROM` join,
    /// where it is not, a bare `*` is refused before this runs.
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
/// `(column, value)` with the column folded by [`fold_ident`], and its predicate. The first two are
/// borrowed from the statement, because the value expressions are spliced into the projection
/// unchanged; the predicate is owned, because under `FROM` it is the `EXISTS` [`set_map`] builds
/// around the statement's own.
type Parts<'a> = (&'a TableWithJoins, Vec<(String, &'a Expr)>, Option<Expr>);

/// Read an `UPDATE` into its [`Parts`], refusing every shape the projection does not model.
fn set_map<'a>(cat: &Catalog, u: &'a Update) -> Result<Parts<'a>> {
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
            // per-column expressions is not a rewrite of this shape.
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
        let col = fold_ident(id);
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
    let pred = match &u.from {
        None => u.selection.clone(),
        Some(UpdateTableFromKind::AfterSet(from)) => {
            from_is_a_filter(cat, u, from)?;
            Some(exists(from.clone(), u.selection.clone()))
        }
        Some(UpdateTableFromKind::BeforeSet(_)) => return Err(unsupported("UPDATE FROM ... SET")),
    };
    Ok((&u.table, sets, pred))
}

/// Refuse an `UPDATE ... FROM` whose `FROM` is more than a filter.
///
/// Postgres updates a target row once if some row of the join makes the predicate TRUE, and when
/// several do, it takes the new values from an unspecified one of them. So the statement is the
/// projection of the module docs over the rows `EXISTS (SELECT 1 FROM F WHERE P)` keeps exactly
/// when nothing it computes reads `F`: then every matching row gives the same new values. This
/// refuses a `SET` value or a `RETURNING` item that may read `F` (see [`may_read`]), a bare
/// `RETURNING *`, which expands to `F`'s columns too, and an `F` relation named like the target,
/// whose name would shadow the target's inside the `EXISTS`.
fn from_is_a_filter(cat: &Catalog, u: &Update, from: &[TableWithJoins]) -> Result<()> {
    let qual = target_qual(&u.table)?;
    let mut factors = Vec::new();
    for twj in from {
        using_factors(&twj.relation, &mut factors);
        twj.joins.iter().for_each(|j| using_factors(&j.relation, &mut factors));
    }
    let mut names = Vec::new();
    let mut cols: Option<Vec<String>> = Some(Vec::new());
    for tf in factors {
        let name = factor_name(tf);
        if name.as_deref() == Some(qual.as_str()) {
            return Err(unsupported("UPDATE ... FROM a relation named like the target"));
        }
        names.extend(name);
        let known = match tf {
            TableFactor::Table { name, alias, args: None, .. }
                if alias.as_ref().is_none_or(|a| a.columns.is_empty()) =>
            {
                find_target(cat, &obj_name(name)).map(|i| cat.tables[i].cols.iter().map(|(c, _)| c.clone()))
            }
            _ => None,
        };
        match (&mut cols, known) {
            (Some(all), Some(c)) => all.extend(c),
            _ => cols = None,
        }
    }
    let reads = |e: &Expr| may_read(e, &names, cols.as_deref());
    if u.assignments.iter().any(|a| reads(&a.value)) {
        return Err(unsupported("UPDATE ... FROM with a SET value that may read the FROM list"));
    }
    for item in u.returning.iter().flatten() {
        let bad = match item {
            SelectItem::Wildcard(_) => true,
            SelectItem::QualifiedWildcard(SelectItemQualifiedWildcardKind::ObjectName(n), _) => {
                n.0.last().and_then(|p| p.as_ident()).is_none_or(|q| fold_ident(q) != qual)
            }
            SelectItem::UnnamedExpr(e) | SelectItem::ExprWithAlias { expr: e, .. } => reads(e),
            _ => true,
        };
        if bad {
            return Err(unsupported("UPDATE ... FROM with a RETURNING item that may read the FROM list"));
        }
    }
    Ok(())
}

/// Whether `e` may read one of an `UPDATE`'s `FROM` relations, judged conservatively: a name
/// qualified by one of `names`, a bare name in `cols` (any bare name when `cols` is unknown), a
/// wildcard qualified by one of `names`, or a subquery, whose own scope this does not follow.
fn may_read(e: &Expr, names: &[String], cols: Option<&[String]>) -> bool {
    let from_name = |n: &ObjectName| n.0.last().and_then(|p| p.as_ident()).is_some_and(|q| names.contains(&fold_ident(q)));
    visit_expressions(e, |x| {
        let hit = match x {
            Expr::Identifier(c) => cols.is_none_or(|cs| cs.contains(&fold_ident(c))),
            Expr::CompoundIdentifier(p) => p.len() >= 2 && names.contains(&fold_ident(&p[p.len() - 2])),
            Expr::QualifiedWildcard(n, _) => from_name(n),
            Expr::Subquery(_) | Expr::Exists { .. } | Expr::InSubquery { .. } => true,
            Expr::Function(f) => match &f.args {
                FunctionArguments::List(l) => l.args.iter().any(|a| match a {
                    FunctionArg::Unnamed(FunctionArgExpr::QualifiedWildcard(n))
                    | FunctionArg::Named { arg: FunctionArgExpr::QualifiedWildcard(n), .. } => from_name(n),
                    _ => false,
                }),
                FunctionArguments::Subquery(_) => true,
                FunctionArguments::None => false,
            },
            _ => false,
        };
        if hit {
            ControlFlow::Break(())
        } else {
            ControlFlow::Continue(())
        }
    })
    .is_break()
}

/// Reduce a pair of `UPDATE`s to the pair of projections computing their final tables, and say
/// which of their outputs each side's `SET` stores ([`Stores`]).
fn update_pair(cat: &Catalog, a: &Update, b: &Update) -> Result<(Query, Query, [Stores; 2])> {
    let (ta, sets_a, pred_a) = set_map(cat, a)?;
    let (tb, sets_b, pred_b) = set_map(cat, b)?;
    let (pred_a, pred_b) = (pred_a.as_ref(), pred_b.as_ref());
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
    no_trigger(cat, &name)?;
    // The declared prefix: an `UPDATE` projects the table's own shape, and a system column
    // `catalog::add_system_columns` appended would change its arity. Independent of the fact that
    // the reduction currently runs before that append, so moving either cannot break this.
    let t = &cat.tables[idx];
    let cols: Vec<String> = t.cols[..t.n_declared].iter().map(|(c, _)| c.clone()).collect();
    let types: Vec<String> = t.cols[..t.n_declared].iter().map(|(_, ty)| ty.clone()).collect();
    // An assignment to a column the catalog does not have would simply not appear in the
    // projection — the update would become invisible. Refuse rather than drop it.
    for (col, _) in sets_a.iter().chain(&sets_b) {
        if !cols.contains(col) {
            return Err(schema(format!("UPDATE sets {col}, which {name} does not declare")));
        }
    }

    let (pa, pb) = (project(&cols, &types, &sets_a, pred_a), project(&cols, &types, &sets_b, pred_b));
    // One output per declared column, after the goal tag where there is one, and each column a side
    // assigns is stored there: in both blocks of `two_goals`, which share the projection.
    let stores = |sets: &[(String, &Expr)], pred: Option<&Expr>| Stores {
        columns: returning
            .then_some(None)
            .into_iter()
            .chain((0..t.n_declared).map(|k| sets.iter().any(|(c, _)| *c == cols[k]).then(|| Store::of(t, k))))
            .collect(),
        guarded: pred.is_some(),
    };
    let stored = [stores(&sets_a, pred_a), stores(&sets_b, pred_b)];
    // Without a `RETURNING` clause the final table is the whole observable and one goal says it all.
    if !returning {
        return Ok((select(pa, ta.clone(), None), select(pb, tb.clone(), None), stored));
    }
    Ok((two_goals(pa, ta, pred_a), two_goals(pb, tb, pred_b), stored))
}

/// The catalog index of a DML target, as [`Catalog::resolve`] finds it: by its name as written, then
/// by its bare name.
///
/// That is how [`resolve_tables`][crate::normalize::resolve_tables] resolves the queries' own
/// references before [`lower`][crate::lower] reads them, so this resolves the target the same way the
/// rest of the pipeline will. Where the pair's guards keep a reference qualified (a system schema, or
/// one bare name reached through two qualifiers) `lower` then fails to resolve the target and the pair
/// is refused, so a mismatch here costs a refusal and never a proof.
fn find_target(cat: &Catalog, name: &str) -> Option<usize> {
    cat.resolve(name)
}

/// Refuse a DML statement on a table a trigger or rule names ([`Table::has_trigger`]): the
/// reductions compare what the two statements write, and a trigger or rule changes what they store,
/// or writes on its own. Checked once the pair is known to have one target, so either side's name
/// will do.
fn no_trigger(cat: &Catalog, name: &str) -> Result<()> {
    match find_target(cat, name) {
        Some(i) if cat.tables[i].has_trigger => {
            Err(unsupported(format!("DML on {}, which a trigger or rule names", cat.tables[i].name)))
        }
        _ => Ok(()),
    }
}

/// One side's projection: the value each column of the target ends up with.
///
/// Returned rather than wrapped in a query, because both blocks of [`two_goals`] use the *same*
/// projection — which is what makes their slot types agree without a cast anywhere.
///
/// An assignment to a temporal column is wrapped in a cast to the column's type: Postgres applies
/// that cast on assignment, so `SET d = ts` stores `ts::date`, and `SET ts = $1::timestamptz` stores
/// `($1::timestamptz)::timestamp`. The cast is written, not implied, because the lowering has no
/// assignment context of its own; a value already of the column's type makes it the identity, which
/// the lowering drops.
///
/// Only where the value's type is evident from its shape ([`type_is_evident`]). Elsewhere the cast
/// rewrite of the inferring modes cannot see through the value, and it wraps a cast it cannot type
/// in a symbol keyed on the value's *text*, so two spellings of one `CASE` would stop matching. The
/// assignment is then left as it was, which costs exactness but not soundness: the value keeps its
/// own type, and the conversions it would have needed stay uninterpreted either way.
fn project(cols: &[String], types: &[String], sets: &[(String, &Expr)], pred: Option<&Expr>) -> Vec<SelectItem> {
    cols
        .iter()
        .zip(types)
        .map(|(c, ty)| {
            let old = Expr::Identifier(spelled(c));
            let assigned = |value: &Expr| match crate::types::temporal_data_type(ty) {
                Some(data_type) if type_is_evident(value) => Expr::Cast {
                    kind: CastKind::Cast,
                    expr: Box::new(value.clone()),
                    data_type,
                    format: None,
                },
                _ => value.clone(),
            };
            let expr = match sets.iter().find(|(set, _)| set == c) {
                None => old,
                // No `WHERE` means every row is updated, so the assigned expression is the value
                // unconditionally and the `CASE` would have an unreachable `ELSE`.
                Some((_, value)) => match pred {
                    None => assigned(value),
                    Some(p) => Expr::Case {
                        case_token: AttachedToken::empty(),
                        end_token: AttachedToken::empty(),
                        operand: None,
                        conditions: vec![CaseWhen {
                            condition: p.clone(),
                            result: assigned(value),
                        }],
                        else_result: Some(Box::new(old)),
                    },
                },
            };
            // Aliased even where the expression is the bare column, so the output names are the
            // catalog's and do not depend on how `lower` names an unaliased projection item.
            SelectItem::ExprWithAlias { expr, alias: spelled(c) }
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
///
/// Each listed column stores the source's output in its position ([`Stores`]).
fn insert_pair(cat: &Catalog, a: &Insert, b: &Insert) -> Result<(Query, Query, [Stores; 2])> {
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
    no_trigger(cat, &name)?;
    let t = &cat.tables[idx];
    // The declared prefix, for the reason [`update_pair`] gives: a system column is not written by
    // an `INSERT` and would look to the guard like an omitted one.
    let declared = &t.cols[..t.n_declared];
    // Both sides are folded as Postgres folds a name: the list by `insert_shape`, the catalog by
    // `catalog::scan_ddl`. A listed column the catalog does not declare is refused rather than
    // ignored: it is written by the statement and invisible to the guard, which would then compute
    // the omitted set wrongly.
    for c in &ca {
        if !declared.iter().any(|(d, _)| d == c) {
            return Err(schema(format!("INSERT lists {c}, which {name} does not declare")));
        }
    }
    for (k, (col, _)) in declared.iter().enumerate() {
        if ca.contains(col) || t.row_determined[k] {
            continue;
        }
        return Err(unsupported(format!(
            "INSERT omits {col}, whose default is not a function of the row"
        )));
    }
    let columns: Vec<Option<Store>> =
        ca.iter().map(|c| declared.iter().position(|(d, _)| d == c).map(|k| Store::of(t, k))).collect();
    let stored = Stores { columns, guarded: false };
    Ok((sa.clone(), sb.clone(), [stored.clone(), stored]))
}

/// Where a reduced `UPDATE` or `INSERT` stores the outputs of the query it became, which is what
/// [`refuse_observed_stores`] reads once that query is lowered. See the module docs.
#[derive(Clone, Debug, Default)]
pub struct Stores {
    /// Parallel to the query's outputs: the column each one is stored in, or `None` for an output
    /// no assignment stores -- an `UPDATE`'s goal tag, and every column its `SET` does not name.
    columns: Vec<Option<Store>>,
    /// Whether each stored value is the `THEN` of the `CASE` an `UPDATE`'s `WHERE` became
    /// ([`project`]): the `ELSE` is the column's old value, which is not stored anew.
    guarded: bool,
}

/// A column a reduction stores a value in.
#[derive(Clone, Debug)]
struct Store {
    name: String,
    /// The column's type as the catalog maps it, and as the DDL spells it.
    ty: String,
    declared: String,
}

impl Store {
    fn of(t: &Table, k: usize) -> Store {
        Store {
            name: t.cols[k].0.clone(),
            ty: t.cols[k].1.clone(),
            declared: t.declared_types.get(k).cloned().unwrap_or_default(),
        }
    }

    /// Whether the column's declared type coerces an interval stored in it by a modifier, which does
    /// not give equal results on intervals `=` calls equal (see the module docs): `interval day`,
    /// `interval hour to minute`, `interval(0)`, or an array of one. A type no DDL spelled is taken
    /// to.
    fn coerces(&self) -> bool {
        if !matches!(coarse_class(&self.ty), Some("interval" | "interval[]")) {
            return false;
        }
        let up = self.declared.to_uppercase();
        let element = up.split('[').next().unwrap_or(&up).trim();
        let element = element.strip_suffix(" ARRAY").unwrap_or(element).trim();
        self.declared.is_empty()
            || element.strip_prefix("INTERVAL").is_some_and(|rest| rest.starts_with([' ', '(']))
    }

    /// The cast that stores a value in this column, for a refusal's message.
    fn cast(&self) -> String {
        let ty = if self.declared.is_empty() { &self.ty } else { &self.declared };
        format!("the assignment cast to {ty} that stores it in {}", self.name)
    }
}

/// Refuse a pair whose reduced `UPDATE` or `INSERT` stores a value of a type whose `=` is not
/// identity through an assignment cast not known to give equal results on values `=` calls equal,
/// or read the value through `q_exact_<type>` where its spelling fixes it. See the module docs.
///
/// `stores` is what [`reduce`] returned, and `types` the output types of each lowered query. Runs
/// on the lowered input, before [`equality::refuse_observed`][crate::equality::refuse_observed],
/// which then finds each stored value already read by its spelling.
///
/// The value stored in an output is found where one expression computes it for every row: a
/// projection's item -- under an `UPDATE`'s `WHERE`, the `THEN` of its `CASE` -- a `VALUES` row's
/// cell, and the same in each branch of a `UNION ALL`, which adds its branches' rows. Below a
/// `DISTINCT`, a slice, or a bare `SELECT *`, an output is a value read from a row, so the pair is
/// refused, as it would be for a projection's item that reads one.
pub fn refuse_observed_stores(input: &mut Value, stores: &[Stores; 2], types: &[Vec<String>; 2]) -> Result<()> {
    let Some(queries) = input.get_mut("queries").and_then(Value::as_array_mut) else { return Ok(()) };
    for ((q, stored), types) in queries.iter_mut().zip(stores).zip(types) {
        for (k, store) in stored.columns.iter().enumerate() {
            let Some(store) = store else { continue };
            // An output whose type's `=` is identity holds no value whose `=` is not: a set
            // operation's and a `VALUES` list's column takes its type from the first branch or row,
            // and one that would hide such a value under another type is refused in `lower`.
            let Some(class) = types.get(k).and_then(|t| coarse_class(t)) else { continue };
            if crate::equality::stores_by_value(&store.ty, store.coerces()) {
                continue;
            }
            stored_at(q, k, stored.guarded, &store.cast(), class)?;
        }
    }
    Ok(())
}

/// Apply [`refuse_observed_stores`]'s rule to every value the relation `rel` computes for its
/// output `k`. `class` is the output's, for a refusal where no one expression computes it.
fn stored_at(rel: &mut Value, k: usize, guarded: bool, cast: &str, class: &str) -> Result<()> {
    let read = |v: &mut Value| crate::equality::read_stored(v, cast);
    if let Some(Value::Array(branches)) = rel.get_mut("union") {
        return branches.iter_mut().try_for_each(|b| stored_at(b, k, guarded, cast, class));
    }
    if let Some(Value::Array(rows)) = rel.pointer_mut("/values/content") {
        return rows.iter_mut().filter_map(|row| row.get_mut(k)).try_for_each(read);
    }
    let Some(item) = rel.pointer_mut(&format!("/project/target/{k}")) else {
        return Err(crate::equality::observed(cast, class));
    };
    if !guarded {
        return read(item);
    }
    // `CASE WHEN p THEN v ELSE c END`, as `lower` builds it: `[p, v, c]`. Any other shape is not
    // the projection this module wrote, and nothing here says which part of it is stored.
    let case = item.get("operator").and_then(Value::as_str) == Some("CASE")
        && item.get("operand").and_then(Value::as_array).is_some_and(|ops| ops.len() == 3);
    match item.pointer_mut("/operand/1") {
        Some(value) if case => read(value),
        _ => Err(crate::equality::observed(cast, class)),
    }
}

/// Whether the cast rewrite can type `e` from its shape alone: a column, a literal or parameter, or a
/// cast (which states its own result type), under any parentheses. See [`project`].
fn type_is_evident(e: &Expr) -> bool {
    match e {
        Expr::Nested(inner) => type_is_evident(inner),
        Expr::Identifier(_) | Expr::CompoundIdentifier(_) | Expr::Value(_) | Expr::Cast { .. } => true,
        _ => false,
    }
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
/// parser whose precedence handling [has had to be guarded][crate::normalize] in the first place.
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

/// `EXISTS (SELECT 1 FROM <from> [WHERE <selection>])`, from the same kind of constant skeleton as
/// [`select`] and for the same reasons.
fn exists(from: Vec<TableWithJoins>, selection: Option<Expr>) -> Expr {
    let mut parsed =
        Parser::parse_sql(&crate::DIALECT, "SELECT 1 FROM t").expect("the skeleton is a constant");
    let Some(Statement::Query(mut q)) = parsed.pop() else {
        unreachable!("the skeleton is one query")
    };
    let SetExpr::Select(select) = &mut *q.body else { unreachable!("the skeleton is a SELECT") };
    select.from = from;
    select.selection = selection;
    Expr::Exists { subquery: q, negated: false }
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
                declared_types: vec!["INTEGER".to_string(); cols.len()],
                nullable: vec![true; cols.len()],
                opaque_identity: vec![false; cols.len()],
                row_determined: cols.iter().map(|c| !vol.contains(c)).collect(),
                keys: Vec::new(),
                primary_key: Vec::new(),
                has_trigger: false,
                n_declared: cols.len(),
                collations: vec![crate::collation::Collation::Default; cols.len()],
            }],
            unread: Vec::new(),
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
    fn a_with_binding_named_like_the_target_is_refused() {
        // `WITH t AS (..) DELETE FROM t` empties the table `t`: the target names the table, and only
        // the statement's other mentions of `t` mean the binding. Reduced, the target would be
        // inlined as the binding and lower like `DELETE FROM t WHERE a = 1`.
        let cat = catalog("t", &["a", "b"]);
        let shadow = "WITH binding named like the DML target";
        let e = err(&cat, "WITH t AS (SELECT * FROM t WHERE a = 1) DELETE FROM t; DELETE FROM t WHERE a = 1;");
        assert!(e.contains(shadow), "{e}");
        let e = err(&cat, "WITH t AS (SELECT a, 0 AS b FROM t) UPDATE t SET b = b; UPDATE t SET b = 0;");
        assert!(e.contains(shadow), "{e}");
        // A qualifier on the target does not keep them apart: it is stripped later.
        let e = err(&cat, "WITH t AS (SELECT * FROM t WHERE a = 1) DELETE FROM s.t; DELETE FROM s.t;");
        assert!(e.contains(shadow), "{e}");
        // A binding under another name is carried across as before.
        assert!(reduced(&cat, "WITH x AS (SELECT a FROM t) DELETE FROM t WHERE a IN (SELECT a FROM x); DELETE FROM t;").is_ok());
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
    fn a_join_delete_is_a_semi_join() {
        let cat = catalog("t", &["a"]);
        assert_eq!(
            reduced(&cat, "DELETE FROM t USING u WHERE t.a = u.a; DELETE FROM t;").unwrap()[0],
            "SELECT * FROM t WHERE EXISTS (SELECT 1 FROM u WHERE t.a = u.a)"
        );
        // A `RETURNING` item that may read `u` is computed from whichever `u` row matched.
        for (sql, want) in [
            ("DELETE FROM t USING u WHERE t.a = u.a RETURNING *; DELETE FROM t RETURNING *;", "RETURNING item"),
            ("DELETE FROM t USING u WHERE t.a = u.a RETURNING u.a; DELETE FROM t RETURNING a;", "RETURNING item"),
            ("DELETE FROM t USING u AS t WHERE t.a = 1; DELETE FROM t;", "named like the target"),
        ] {
            assert!(err(&cat, sql).contains(want), "{sql}");
        }
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
    fn row_assignment_and_a_join_update_reading_the_join_are_refused() {
        let cat = catalog("t", &["a", "b"]);
        assert!(err(&cat, "UPDATE t SET (a, b) = (SELECT 1, 2); UPDATE t SET a = 1;")
            .contains("row assignment"));
        // Which `u` row supplies the value is unspecified when several match.
        assert!(err(&cat, "UPDATE t SET a = u.a FROM u; UPDATE t SET a = 1;")
            .contains("SET value that may read the FROM list"));
        // A `FROM` that only filters is a semi-join.
        let r = reduced(&cat, "UPDATE t SET a = 1 FROM u WHERE t.b = u.b; UPDATE t SET a = 1;").unwrap();
        assert!(r[0].contains("CASE WHEN EXISTS (SELECT 1 FROM u WHERE t.b = u.b) THEN 1 ELSE a END"), "{}", r[0]);
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
