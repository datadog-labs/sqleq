// Unless explicitly stated otherwise all files in this repository are licensed under the
// Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/).
// Copyright 2026-Present Datadog, Inc.

//! A pair of `INSERT`s, as the Lean checker sees it.
//!
//! [`translate`] orients the pair so that A is the `VALUES` side, applies every precondition of
//! `Sqleq.checkGather` in Rust so that a refusal carries a reason, and interns every name to a
//! number. The kernel checks all of those preconditions again: a translator bug that let a pair
//! through here would be refused there, not proved.

use std::collections::HashMap;

use sqlparser::ast::{ConflictTarget, OnConflictAction, OnInsert, Statement};

use crate::recognize::{recognize, Cell, Parts, Refusal, Source, Tail};
use crate::schema::{fold, last_name, type_key, DefaultKind, Schema, Table, TypeKey};

/// The `VALUES` side's sub-shape, reported so that one shape cannot hide inside another's numbers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Shape {
    /// One row.
    SingleRow,
    /// Every row is the same tuple.
    Repeated,
    /// Only parameters, numbered `$1..$(k·n)` in reading order.
    RowMajor,
    Other,
}

impl Shape {
    pub fn name(self) -> &'static str {
        match self {
            Shape::SingleRow => "single-row",
            Shape::Repeated => "repeated-tuple",
            Shape::RowMajor => "row-major",
            Shape::Other => "other",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LCell {
    P(u32),
    Pc(u32, u32),
    Null,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LTok {
    Word(u32),
    Param(u32),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LSource {
    Values(Vec<Vec<LCell>>),
    /// `(param, element type id)` per argument.
    Unnest(Vec<(u32, u32)>),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LInsert {
    pub target: u32,
    pub cols: Vec<u32>,
    pub src: LSource,
    pub tail: Vec<LTok>,
}

/// The witness model's view of the target table and conflict clause (`Sqleq.Spec`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LSpec {
    /// `(nullable, default, always)` per table column, in table order.
    pub cols: Vec<(bool, DefaultKind, bool)>,
    /// `(key columns, nulls not distinct)` per unique constraint.
    pub uniques: Vec<(Vec<usize>, bool)>,
    /// The insert's column list as table column indices.
    pub ins: Vec<usize>,
    pub conflict: LConflict,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LConflict {
    None,
    Nothing,
    NothingOn(usize),
    Update(usize),
    NoArbiter,
}

/// What the witness can and cannot speak for.
#[derive(Clone, Debug)]
pub enum Witness {
    /// Run the canonical run on this spec.
    Spec(LSpec),
    /// No witness is attempted, for this reason. The pair cannot be credited.
    Skip(String),
}

/// A pair ready to emit: A is `VALUES`, B is `unnest`.
#[derive(Clone, Debug)]
pub struct LeanPair {
    /// Declared type id of each target column, in column-list order.
    pub tys: Vec<u32>,
    pub a: LInsert,
    pub b: LInsert,
    pub shape: Shape,
    /// The input's first statement was the `unnest` side.
    pub flipped: bool,
    pub witness: Witness,
    /// Constraints the witness model does not check: `check`, `foreign-key`.
    pub unmodelled: Vec<&'static str>,
    /// Table column names and unique constraint descriptions, for reporting a failed witness.
    pub col_names: Vec<String>,
    pub unique_names: Vec<String>,
    /// What the Postgres replay needs and the Lean side does not.
    pub replay: Replay,
}

/// The target as the SQL names it, and every table column's type and default as the DDL writes
/// them, for the Postgres replay.
#[derive(Clone, Debug)]
pub struct Replay {
    pub target_sql: String,
    /// The insert's column list, folded, in order.
    pub insert: Vec<String>,
    /// `(name, type, default, listed by the INSERT)` per table column, in table order.
    pub columns: Vec<(String, String, Option<String>, bool)>,
}

/// Resolve the conflict clause to what the witness model needs: which unique constraint is the
/// arbiter, the way Postgres infers it.
fn conflict(on: &Option<OnInsert>, table: &Table) -> LConflict {
    let Some(on) = on else { return LConflict::None };
    let OnInsert::OnConflict(oc) = on else { return LConflict::NoArbiter };
    let arbiter = match &oc.conflict_target {
        None => None,
        Some(ConflictTarget::Columns(ids)) => {
            let mut want: Vec<usize> = Vec::new();
            for id in ids {
                match table.column(&fold(id)) {
                    Some(i) => want.push(i),
                    None => return LConflict::NoArbiter,
                }
            }
            want.sort_unstable();
            // Inference: a unique index on exactly these columns, not partial (Postgres needs a
            // matching `WHERE`, which this fragment never has) and not on expressions.
            let found = table.uniques.iter().position(|u| {
                let mut c = u.cols.clone();
                c.sort_unstable();
                !u.partial && !u.expr && c == want
            });
            match found {
                Some(u) => Some(u),
                None => return LConflict::NoArbiter,
            }
        }
        Some(ConflictTarget::OnConstraint(name)) => {
            let want = last_name(name);
            match table.uniques.iter().position(|u| u.name.is_some() && u.name == want) {
                Some(u) => Some(u),
                None => return LConflict::NoArbiter,
            }
        }
    };
    match (&oc.action, arbiter) {
        (OnConflictAction::DoNothing, None) => LConflict::Nothing,
        (OnConflictAction::DoNothing, Some(u)) => LConflict::NothingOn(u),
        (OnConflictAction::DoUpdate(_), Some(u)) => LConflict::Update(u),
        // `DO UPDATE` without a target is rejected by Postgres.
        (OnConflictAction::DoUpdate(_), None) => LConflict::NoArbiter,
    }
}

/// Above this many `VALUES` rows no witness is attempted, so the pair gets no credit. The canonical
/// run checks every row against every earlier one, so its kernel time grows with the square of the
/// row count: a generated 200-row, 15-column pair with two unique keys takes about 13 s, and past a
/// few hundred rows it takes minutes.
pub const WITNESS_MAX_ROWS: usize = 500;

fn witness_for(a: &Parts, table: &Table) -> Witness {
    if let Source::Values(rows) = &a.src {
        if rows.len() > WITNESS_MAX_ROWS {
            return Witness::Skip(format!(
                "witness: {} VALUES rows is over the kernel witness limit of {WITNESS_MAX_ROWS}",
                rows.len()
            ));
        }
    }
    if table.has_exclude {
        return Witness::Skip("schema: an EXCLUDE constraint on the target table is not modelled".into());
    }
    if table.unread {
        return Witness::Skip("schema: a DDL statement naming the target table could not be read".into());
    }
    if table.altered {
        return Witness::Skip("schema: the DDL alters the target table, and ALTER TABLE is not read".into());
    }
    if table.retried {
        return Witness::Skip(
            "schema: the table's DDL only parsed after simplification, which can drop NOT NULL".into(),
        );
    }
    let ins = a.cols.iter().map(|c| table.column(c).expect("resolved above")).collect();
    Witness::Spec(LSpec {
        cols: table.columns.iter().map(|c| (c.nullable, c.default, c.always)).collect(),
        uniques: table.uniques.iter().map(|u| (u.cols.clone(), u.nnd)).collect(),
        ins,
        conflict: conflict(&a.on, table),
    })
}

#[derive(Default)]
struct Interner(HashMap<String, u32>);

impl Interner {
    fn id(&mut self, key: String) -> u32 {
        let n = self.0.len() as u32;
        *self.0.entry(key).or_insert(n)
    }
    fn ty(&mut self, t: &TypeKey) -> u32 {
        self.id(format!("ty:{}", t.name))
    }
}

/// Split the tail into words and parameters. A `$` followed by digits is a parameter wherever it
/// appears, including inside a string literal: that can only refuse a pair, never admit one.
fn tokens(tail: &Tail, interner: &mut Interner) -> Vec<LTok> {
    let text = tail.text();
    let mut out = Vec::new();
    let mut word = String::new();
    let mut chars = text.chars().peekable();
    let flush = |w: &mut String, out: &mut Vec<LTok>, i: &mut Interner| {
        if !w.is_empty() {
            out.push(LTok::Word(i.id(format!("w:{w}"))));
            w.clear();
        }
    };
    while let Some(c) = chars.next() {
        if c == '$' && chars.peek().is_some_and(|d| d.is_ascii_digit()) {
            flush(&mut word, &mut out, interner);
            let mut n = String::new();
            while let Some(d) = chars.peek().filter(|d| d.is_ascii_digit()) {
                n.push(*d);
                chars.next();
            }
            out.push(LTok::Param(n.parse().unwrap_or(u32::MAX)));
        } else if c.is_whitespace() {
            flush(&mut word, &mut out, interner);
        } else {
            word.push(c);
        }
    }
    flush(&mut word, &mut out, interner);
    out
}

fn shape(rows: &[Vec<Cell>]) -> Shape {
    if rows.len() == 1 {
        return Shape::SingleRow;
    }
    if rows.iter().all(|r| r == &rows[0]) {
        return Shape::Repeated;
    }
    let mut next = 1;
    for c in rows.iter().flatten() {
        match c {
            Cell::Param(n, _) if *n == next => next += 1,
            _ => return Shape::Other,
        }
    }
    Shape::RowMajor
}

fn unsupported(r: impl Into<String>) -> Refusal {
    Refusal::Unsupported(r.into())
}

/// Translate a pair of statements, in the order the input gives them.
pub fn translate(first: &Statement, second: &Statement, schema: &Schema) -> Result<LeanPair, Refusal> {
    let p1 = recognize(first)?;
    let p2 = recognize(second)?;
    let (a, b, flipped) = match (&p1.src, &p2.src) {
        (Source::Values(_), Source::Unnest { .. }) => (p1, p2, false),
        (Source::Unnest { .. }, Source::Values(_)) => (p2, p1, true),
        (Source::Values(_), Source::Values(_)) => return Err(unsupported("both sides are VALUES")),
        (Source::Unnest { .. }, Source::Unnest { .. }) => return Err(unsupported("both sides are unnest")),
    };
    let (tys, la, lb, shape) = lower(&a, &b, schema)?;
    let table = schema.table(&a.target).expect("resolved by lower");
    let mut unmodelled = Vec::new();
    if table.has_check {
        unmodelled.push("check");
    }
    if table.has_fk {
        unmodelled.push("foreign-key");
    }
    if table.has_trigger {
        unmodelled.push("trigger");
    }
    Ok(LeanPair {
        tys,
        a: la,
        b: lb,
        shape,
        flipped,
        witness: witness_for(&a, table),
        unmodelled,
        replay: Replay {
            target_sql: a.target_sql(),
            insert: a.cols.clone(),
            columns: table
                .columns
                .iter()
                .map(|c| (c.name.clone(), c.raw_type.clone(), c.default_text.clone(), a.cols.contains(&c.name)))
                .collect(),
        },
        col_names: table.columns.iter().map(|c| c.name.clone()).collect(),
        unique_names: table
            .uniques
            .iter()
            .map(|u| {
                let cols: Vec<&str> = u.cols.iter().map(|&i| table.columns[i].name.as_str()).collect();
                match &u.name {
                    Some(n) => format!("{n} ({})", cols.join(", ")),
                    None => format!("unique index on ({})", cols.join(", ")),
                }
            })
            .collect(),
    })
}

fn lower(a: &Parts, b: &Parts, schema: &Schema) -> Result<(Vec<u32>, LInsert, LInsert, Shape), Refusal> {
    let (Source::Values(rows), Source::Unnest { args, .. }) = (&a.src, &b.src) else {
        unreachable!("oriented by translate")
    };
    if a.target != b.target {
        return Err(unsupported("the two sides insert into different tables"));
    }
    if a.cols != b.cols {
        return Err(unsupported("the two sides' column lists differ"));
    }
    if a.tail != b.tail {
        return Err(unsupported("the two sides' alias, conflict clause or RETURNING differ"));
    }
    let table = schema.table(&a.target).map_err(|e| unsupported(format!("schema: {e}")))?;
    let k = a.cols.len();

    // Postgres rejects these outright, so there is no equivalence question.
    for r in rows {
        if r.len() != k {
            return Err(Refusal::InvalidSql(format!(
                "a VALUES row has {} expressions for {k} target columns",
                r.len()
            )));
        }
    }
    if args.len() != k {
        return Err(Refusal::InvalidSql(format!(
            "unnest has {} arrays for {k} target columns",
            args.len()
        )));
    }

    let mut interner = Interner::default();
    let mut tys = Vec::with_capacity(k);
    let mut col_ids = Vec::with_capacity(k);
    for c in &a.cols {
        let Some(i) = table.column(c) else {
            return Err(unsupported(format!("schema: {c} is not a column of {}", table.name)));
        };
        let ty = &table.columns[i].ty;
        // A modifier on the column is fine: both sides reach it through the same assignment
        // coercion from the base type. An array column is not: `unnest` flattens every dimension,
        // so no gather produces it.
        if ty.array {
            return Err(unsupported(format!("column {c} has an array type")));
        }
        tys.push(interner.ty(ty));
        col_ids.push(interner.id(format!("c:{c}")));
    }

    let mut lb_args = Vec::with_capacity(k);
    for (j, arg) in args.iter().enumerate() {
        if arg.param as usize != j + 1 {
            return Err(unsupported("unnest arguments are not $1..$k in order"));
        }
        let elem = type_key(&arg.elem);
        if elem.modded {
            return Err(unsupported(format!(
                "unnest argument ${} is cast to {}[]: an explicit cast with a modifier truncates",
                arg.param, arg.elem
            )));
        }
        if interner.ty(&elem) != tys[j] || elem.array {
            return Err(unsupported(format!(
                "unnest argument ${} is {}[], not the type of column {}",
                arg.param, arg.elem, a.cols[j]
            )));
        }
        lb_args.push((arg.param, tys[j]));
    }

    let mut la_rows = Vec::with_capacity(rows.len());
    for r in rows {
        let mut out = Vec::with_capacity(k);
        for (j, c) in r.iter().enumerate() {
            let n = match c {
                Cell::Null => {
                    out.push(LCell::Null);
                    continue;
                }
                Cell::Param(n, _) => *n,
            };
            if n == 0 || (n as usize - 1) % k != j {
                return Err(unsupported(format!(
                    "${n} is not pinned to one column (it is in column {}, numbered for column {})",
                    j + 1,
                    if n == 0 { 0 } else { (n as usize - 1) % k + 1 }
                )));
            }
            match c {
                Cell::Param(_, None) => out.push(LCell::P(n)),
                Cell::Param(_, Some(dt)) => {
                    let t = type_key(dt);
                    let id = interner.ty(&t);
                    if id != tys[j] || !t.plain() {
                        return Err(unsupported(format!(
                            "${n} is cast to {dt}, not the type of column {}",
                            a.cols[j]
                        )));
                    }
                    out.push(LCell::Pc(n, id));
                }
                Cell::Null => unreachable!(),
            }
        }
        la_rows.push(out);
    }

    let tail_a = tokens(&a.tail, &mut interner);
    if tail_a.iter().any(|t| matches!(t, LTok::Param(_))) {
        return Err(unsupported("a parameter in the conflict clause or RETURNING"));
    }
    // Each side's target, columns and tail are interned from *that side's own* text, not copied from
    // A, so the kernel's equality checks compare two independent renderings. If the comparisons
    // above had a bug, the kernel would refuse the pair rather than prove it.
    let side = |p: &Parts, i: &mut Interner| {
        let target = i.id(format!("t:{}", p.target));
        let cols: Vec<u32> = p.cols.iter().map(|c| i.id(format!("c:{c}"))).collect();
        let tail = tokens(&p.tail, i);
        (target, cols, tail)
    };
    let (ta, ca, tla) = side(a, &mut interner);
    let (tb, cb, tlb) = side(b, &mut interner);
    debug_assert_eq!(ca, col_ids);
    let la = LInsert { target: ta, cols: ca, src: LSource::Values(la_rows), tail: tla };
    let lb = LInsert { target: tb, cols: cb, src: LSource::Unnest(lb_args), tail: tlb };
    Ok((tys, la, lb, shape(rows)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqlparser::parser::Parser;

    const DDL: &str = "CREATE TABLE t (a int NOT NULL, b text, c varchar(10), d bigint, e text[]);";

    fn pair(a: &str, b: &str) -> Result<LeanPair, Refusal> {
        let p = |s: &str| Parser::parse_sql(&sqleq_frontend::internals::DIALECT, s).unwrap().remove(0);
        translate(&p(a), &p(b), &Schema::from_ddl(DDL))
    }

    const B: &str = "INSERT INTO t (a, b) SELECT * FROM unnest($1::int[], $2::text[])";

    #[test]
    fn row_major_and_repeated() {
        let p = pair("INSERT INTO t (a, b) VALUES ($1, $2), ($3, $4)", B).unwrap();
        assert_eq!(p.shape, Shape::RowMajor);
        assert!(!p.flipped);
        let p = pair("INSERT INTO t (a, b) VALUES ($1, $2), ($1, $2)", B).unwrap();
        assert_eq!(p.shape, Shape::Repeated);
    }

    #[test]
    fn either_order_is_accepted() {
        let p = pair(B, "INSERT INTO t (a, b) VALUES ($1, $2::text)").unwrap();
        assert!(p.flipped);
        assert_eq!(p.shape, Shape::SingleRow);
    }

    #[test]
    fn a_shared_parameter_free_tail_is_accepted() {
        let tail = " ON CONFLICT (a) DO UPDATE SET b = EXCLUDED.b RETURNING a";
        assert!(pair(&format!("INSERT INTO t (a, b) VALUES ($1, $2){tail}"), &format!("{B}{tail}")).is_ok());
    }

    fn refused(a: &str, b: &str) -> Refusal {
        pair(a, b).expect_err(&format!("{a} | {b} was not refused"))
    }

    #[test]
    fn negative_controls() {
        let v = "INSERT INTO t (a, b) VALUES ($1, $2), ($3, $4)";
        // Cast mismatch on either side.
        assert!(matches!(refused(v, "INSERT INTO t (a, b) SELECT * FROM unnest($1::bigint[], $2::text[])"), Refusal::Unsupported(_)));
        assert!(matches!(refused("INSERT INTO t (a, b) VALUES ($1::bigint, $2)", B), Refusal::Unsupported(_)));
        // A parameter in the tail.
        let t = " ON CONFLICT (a) DO UPDATE SET b = $3";
        assert!(refused(&format!("{v}{t}"), &format!("{B}{t}")).reason().contains("parameter"));
        // Different tails.
        assert!(refused(&format!("{v} RETURNING a"), B).reason().contains("differ"));
        // B := A.
        assert!(refused(v, v).reason().contains("both sides are VALUES"));
        // Swapped unnest arguments.
        assert!(refused(v, "INSERT INTO t (a, b) SELECT * FROM unnest($2::text[], $1::int[])").reason().contains("in order"));
        // Reordered column list.
        assert!(refused("INSERT INTO t (b, a) VALUES ($1, $2)", B).reason().contains("column lists differ"));
        // A parameter reused across columns.
        assert!(refused("INSERT INTO t (a, b) VALUES ($1, $1)", B).reason().contains("pinned"));
        // A cast with a type modifier truncates; the same modifier on the column alone does not.
        assert!(refused("INSERT INTO t (a, c) VALUES ($1, $2)", "INSERT INTO t (a, c) SELECT * FROM unnest($1::int[], $2::varchar(10)[])").reason().contains("truncates"));
        assert!(pair("INSERT INTO t (a, c) VALUES ($1, $2)", "INSERT INTO t (a, c) SELECT * FROM unnest($1::int[], $2::varchar[])").is_ok());
        // An array column.
        assert!(refused("INSERT INTO t (a, e) VALUES ($1, $2)", "INSERT INTO t (a, e) SELECT * FROM unnest($1::int[], $2::text[])").reason().contains("array type"));
    }

    #[test]
    fn widths_postgres_rejects_are_invalid_sql() {
        assert!(matches!(refused("INSERT INTO t (a, b) VALUES ($1), ($2)", B), Refusal::InvalidSql(_)));
        assert!(matches!(refused("INSERT INTO t (a, b) VALUES ($1, $2)", "INSERT INTO t (a, b) SELECT * FROM unnest($1::int[])"), Refusal::InvalidSql(_)));
    }

    #[test]
    fn tail_tokens_see_parameters_anywhere() {
        let mut i = Interner::default();
        let t = Tail { alias: None, on: Some("DO UPDATE SET x=$12,y".into()), returning: None };
        assert!(tokens(&t, &mut i).contains(&LTok::Param(12)));
    }
}
