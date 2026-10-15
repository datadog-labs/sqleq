// Unless explicitly stated otherwise all files in this repository are licensed under the
// Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/).
// Copyright 2026-Present Datadog, Inc.

//! A pair of `INSERT`s, as the Lean checker sees it.
//!
//! [`translate`] orients the pair so that A is the `VALUES` side, applies every precondition of
//! `Sqleq.checkGather` (or, for a pair with generated cells, `Sqleq.checkGatherGen`) in Rust so
//! that a refusal carries a reason, and interns every name to a number. The kernel checks all of
//! those preconditions again: a translator bug that let a pair through here would be refused there,
//! not proved.
//!
//! A pair with generated cells has further preconditions that need the schema, which the kernel
//! never sees, so they are checked here only (see [`generated_preconditions`]).

use std::collections::{BTreeMap, HashMap};

use sqlparser::ast::{ConflictTarget, OnConflictAction, OnInsert, Statement};

use crate::recognize::{recognize, Cell, GenKind, Parts, Refusal, Seq, Source, Tail, GENERATORS, SEQUENCE_TYPES};
use crate::schema::{fold, last_name, type_key, DefaultKind, DefaultSource, Schema, Table, TypeKey, Unique};

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
    Gen(GenKind),
    /// `nextval($n)`: a fresh generated cell whose sequence name is parameter `n`.
    SeqP(u32),
}

/// What a pair's generated cells are, for its record: the claim is the same for all of them, but
/// a reader needs to know which pairs bypass a sequence.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct GenSummary {
    /// Generated cells per generator: `default`, `now`, `nextval`, ….
    pub funcs: BTreeMap<String, usize>,
    /// The target columns holding a generated cell, in column-list order.
    pub columns: Vec<String>,
    /// Some column holds a generated cell in one row and a parameter or `NULL` in another.
    pub mixed: bool,
    /// Some generated cell draws from a sequence: an explicit `nextval` (of a name or a parameter),
    /// or `DEFAULT` on a serial, identity or `nextval` default. B then supplies those values itself
    /// and leaves the sequence behind them, so as a rewrite it is not safe, though the claim holds.
    pub sequence: bool,
    /// Some `DEFAULT` cell's column default is a constant or `NULL`, so its value is fixed by the
    /// schema.
    pub constant_default: bool,
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
    /// The `VALUES` side has generated cells, so the pair is checked by `checkGatherGen`.
    pub generated: Option<GenSummary>,
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
    /// A's `VALUES` clause as written, which the replay runs into a copy of the table to learn
    /// what its generated cells evaluate to.
    pub values_text: String,
    /// `(name, type, default, listed by the INSERT)` per table column, in table order.
    pub columns: Vec<(String, String, Option<String>, bool)>,
    /// Positions in the insert's column list whose unnest element type is not the column's own but
    /// one that holds its every value (`int8[]` into an `int4` column). The replay runs these pairs
    /// once more with values at the limits of the column's type.
    pub widened: Vec<usize>,
}

/// Resolve the conflict clause to what the witness model needs: which unique constraint is the
/// arbiter, the way Postgres infers it.
///
/// Postgres never takes a deferrable constraint as an arbiter. Where it would have to, it raises
/// before inserting a row ("ON CONFLICT does not support deferrable unique constraints/exclusion
/// constraints as arbiters"), which is `NoArbiter` here:
/// - a column target that any deferrable constraint on exactly those columns matches, even beside
///   one that is not deferrable;
/// - `ON CONSTRAINT` naming a deferrable constraint (only the named one is the arbiter, so a
///   deferrable twin on the same columns does not matter);
/// - a target-less `DO NOTHING` on a table with any deferrable unique constraint, since without a
///   target every unique index is an arbiter.
fn conflict(on: &Option<OnInsert>, table: &Table) -> LConflict {
    let Some(on) = on else { return LConflict::None };
    let OnInsert::OnConflict(oc) = on else { return LConflict::NoArbiter };
    let arbiter = match &oc.conflict_target {
        None => {
            if table.uniques.iter().any(|u| u.deferrable) {
                return LConflict::NoArbiter;
            }
            None
        }
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
            let matches = |u: &&Unique| {
                let mut c = u.cols.clone();
                c.sort_unstable();
                !u.partial && !u.expr && c == want
            };
            if table.uniques.iter().filter(matches).any(|u| u.deferrable) {
                return LConflict::NoArbiter;
            }
            match table.uniques.iter().position(|u| matches(&u)) {
                Some(u) => Some(u),
                None => return LConflict::NoArbiter,
            }
        }
        Some(ConflictTarget::OnConstraint(name)) => {
            let want = last_name(name);
            match table.uniques.iter().position(|u| u.name.is_some() && u.name == want) {
                Some(u) if table.uniques[u].deferrable => return LConflict::NoArbiter,
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

/// The types `Sqleq.widens` names, in the order of the ids `lean/Sqleq/Check.lean` gives them
/// (`tyInt2 = 0`, …, `tyVarchar = 4`). [`Interner::new`] pins them to those ids.
const PINNED_TYPES: [&str; 5] = ["int2", "int4", "int8", "text", "varchar"];

/// `(element, column)` base types for which an unnest element may differ from its column's type: the
/// element type holds every value of the column's type, and assigning it back gives the same value.
/// The kernel checks the same list, `Sqleq.widens`, by id.
const WIDENS: &[(&str, &str)] =
    &[("int8", "int4"), ("int8", "int2"), ("int4", "int2"), ("text", "varchar"), ("varchar", "text")];

struct Interner(HashMap<String, u32>);

impl Interner {
    fn new() -> Interner {
        let mut i = Interner(HashMap::new());
        for t in PINNED_TYPES {
            i.id(format!("ty:{t}"));
        }
        i
    }
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
    // A generated cell takes no parameter number, so it does not break row-major numbering; the
    // sequence name of `nextval($n)` does take one.
    let mut next = 1;
    for c in rows.iter().flatten() {
        match (c, c.number()) {
            (_, Some(n)) if n == next => next += 1,
            (Cell::Gen(_), None) => {}
            _ => return Shape::Other,
        }
    }
    Shape::RowMajor
}

/// `Sqleq.pinSeq`: with generated cells, the parameters must be `$1, $2, …` in reading order, each
/// exactly once. `NULL` and generated cells take no number, except the sequence name of
/// `nextval($n)` ([`Cell::number`]).
fn pin_seq(rows: &[Vec<Cell>]) -> Result<(), Refusal> {
    let mut next = 1;
    for c in rows.iter().flatten() {
        if let Some(n) = c.number() {
            if n != next {
                return Err(unsupported(format!(
                    "${n} is out of reading order, where ${next} was expected: with generated cells, \
                     the parameters must be $1, $2, … in reading order, each once"
                )));
            }
            next += 1;
        }
    }
    Ok(())
}

/// Every parameter from `$1` to the highest is used. Slot pinning allows a gap (`($1, $2), ($5,
/// $6)`), but a parameter no cell uses has nothing to take its type from, so Postgres cannot
/// prepare the statement and the `VALUES` side never runs. Checked here only: it is about what
/// Postgres will prepare, which the kernel's model does not see. (`pin_seq` rules gaps out for a
/// pair with generated cells.)
fn no_gaps(rows: &[Vec<Cell>]) -> Result<(), Refusal> {
    let mut used: Vec<u32> = rows
        .iter()
        .flatten()
        .filter_map(|c| match c {
            Cell::Param(n, _) => Some(*n),
            _ => None,
        })
        .collect();
    used.sort_unstable();
    used.dedup();
    match used.iter().zip(1..).find(|(n, i)| **n != *i) {
        Some((_, missing)) => Err(unsupported(format!(
            "${missing} is never used, so Postgres cannot infer its type and the VALUES side does not prepare"
        ))),
        None => Ok(()),
    }
}

/// Every word of a tail, lower-cased, split on anything that cannot be part of an identifier.
fn words(text: &str) -> impl Iterator<Item = String> + '_ {
    text.split(|c: char| !(c.is_alphanumeric() || c == '_')).filter(|w| !w.is_empty()).map(str::to_lowercase)
}

/// The preconditions of a pair with generated cells that need the schema. The kernel checks only
/// what it sees (types, positions, the tail's text); these are checked here alone, which is why
/// each one refuses rather than approximates. Returns the pair's [`GenSummary`].
///
/// Each closes a way in which a generated cell is *not* the same as B supplying its value:
/// - a `GENERATED ALWAYS` column accepts `DEFAULT` but rejects an explicit value;
/// - a table whose DDL was not read in full may have such a column, or a rule that re-evaluates the
///   generators, without our knowing;
/// - when a generated cell draws from a sequence, A advances it row by row and B does not, so
///   anything else in the statement that reads that sequence (the tail, another column's default,
///   a trigger) sees different values on the two sides. An omitted column's default the checker
///   does not recognise counts as reading it;
/// - a `DEFAULT` whose expression the checker does not recognise could do anything besides give a
///   value;
/// - a generator whose value the column's type cannot take makes A fail where B need not;
/// - two generators in one column (`DEFAULT` from one sequence, `nextval` of another) can produce
///   colliding values the witness model treats as distinct.
///
/// A sequence named by a parameter, `nextval($n)`, could be any sequence, so where a named one is
/// refused for "the same sequence", it is refused for any.
pub fn generated_preconditions(a: &Parts, rows: &[Vec<Cell>], table: &Table) -> Result<GenSummary, Refusal> {
    let mut sum = GenSummary::default();
    let mut seqs: Vec<String> = Vec::new();
    let mut unknown_seq = false;
    for (j, c) in a.cols.iter().enumerate() {
        let col = &table.columns[table.column(c).expect("resolved by lower")];
        let mut gens: Vec<(String, Option<Seq>)> = Vec::new();
        let mut other = false;
        for r in rows {
            let Cell::Gen(g) = &r[j] else {
                other = true;
                continue;
            };
            *sum.funcs.entry(g.func.clone()).or_default() += 1;
            // Every `nextval($n)` in a column is one generator: the witness, and the replay with it,
            // name the same sequence in every sequence-name parameter.
            let key = match &g.seq {
                Some(Seq::Param(_)) => (g.func.clone(), Some(Seq::Param(0))),
                seq => (g.func.clone(), seq.clone()),
            };
            if !gens.contains(&key) {
                gens.push(key);
            }
            if col.always {
                return Err(unsupported(format!(
                    "column {c} is GENERATED ALWAYS: Postgres accepts DEFAULT there, but rejects the \
                     unnest side's explicit value"
                )));
            }
            match g.kind {
                GenKind::Default => match &col.source {
                    DefaultSource::Constant => sum.constant_default = true,
                    DefaultSource::Sequence(s) => seqs.push(s.clone()),
                    DefaultSource::Generator => {}
                    DefaultSource::Other => {
                        return Err(unsupported(format!(
                            "DEFAULT on column {c}, whose default is not a constant, a sequence or a \
                             recognised generator"
                        )))
                    }
                },
                GenKind::Fresh | GenKind::Once => {
                    let types: &[&str] = match &g.seq {
                        Some(Seq::Named(s)) => {
                            seqs.push(s.clone());
                            SEQUENCE_TYPES
                        }
                        Some(Seq::Param(_)) => {
                            unknown_seq = true;
                            SEQUENCE_TYPES
                        }
                        None => GENERATORS.iter().find(|(f, _, _)| *f == g.func).map_or(&[], |x| x.2),
                    };
                    if !types.contains(&col.ty.name.as_str()) {
                        return Err(unsupported(format!(
                            "{}() cannot fill column {c} of type {}",
                            g.func, col.ty.name
                        )));
                    }
                }
            }
        }
        if gens.len() > 1 {
            return Err(unsupported(format!("column {c} is filled by more than one generator")));
        }
        if !gens.is_empty() {
            sum.columns.push(c.clone());
            sum.mixed |= other;
        }
    }
    if table.unread || table.altered || table.retried {
        return Err(unsupported(
            "schema: generated cells need the target table's DDL read in full (a statement naming it \
             could not be read, alters it, or only parsed after simplification)",
        ));
    }
    sum.sequence = !seqs.is_empty() || unknown_seq;
    if sum.sequence {
        let tail = format!("{} {}", a.tail.on.as_deref().unwrap_or(""), a.tail.returning.as_deref().unwrap_or(""));
        if words(&tail).any(|w| matches!(w.as_str(), "nextval" | "currval" | "lastval" | "setval" | "default")) {
            return Err(unsupported(
                "a generated cell draws from a sequence, and the conflict clause or RETURNING reads \
                 sequence state or a default",
            ));
        }
        for col in table.columns.iter().filter(|col| !a.cols.contains(&col.name)) {
            if col.seq_state || col.seqs.iter().any(|s| seqs.contains(s)) {
                return Err(unsupported(format!(
                    "a generated cell draws from a sequence, and the default of omitted column {} \
                     reads the same sequence state",
                    col.name
                )));
            }
            // `sequence_use` sees only what the default writes itself, so a default it does not
            // recognise could call a function that draws from the same sequence. A stored generated
            // column's expression cannot: Postgres requires it to be immutable.
            if col.source == DefaultSource::Other && !col.always {
                return Err(unsupported(format!(
                    "a generated cell draws from a sequence, and the default of omitted column {} is \
                     not recognised, so it could read the same sequence state",
                    col.name
                )));
            }
            // A sequence a parameter names could be any, so drawing from any sequence draws from it.
            if unknown_seq && !col.seqs.is_empty() {
                return Err(unsupported(format!(
                    "a generated cell draws from the sequence a parameter names, which could be any, \
                     and the default of omitted column {} draws from a sequence",
                    col.name
                )));
            }
        }
        if table.has_trigger {
            return Err(unsupported(
                "a generated cell draws from a sequence, and the table has a trigger, which could read it",
            ));
        }
        // The words above see only what the tail writes itself, as `sequence_use` does for a
        // default (#171), and a CHECK is not read at all. A function called from either could read
        // the sequence state the generated cells advance (#178).
        if a.tail_calls {
            return Err(unsupported(
                "a generated cell draws from a sequence, and the conflict clause or RETURNING calls a \
                 function, which could read it",
            ));
        }
        if table.check_calls {
            return Err(unsupported(
                "a generated cell draws from a sequence, and a CHECK on the target calls a function, \
                 which could read it",
            ));
        }
    }
    Ok(sum)
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
    let (a, b) = (implicit_cols(a, schema)?, implicit_cols(b, schema)?);
    let Lowered { tys, a: la, b: lb, shape, generated, widened } = lower(&a, &b, schema)?;
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
        generated,
        unmodelled,
        replay: Replay {
            target_sql: a.target_sql(),
            insert: a.cols.clone(),
            values_text: a.values_text().unwrap_or_default(),
            columns: table
                .columns
                .iter()
                .map(|c| (c.name.clone(), c.raw_type.clone(), c.default_text.clone(), a.cols.contains(&c.name)))
                .collect(),
            widened,
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

/// An INSERT without a column list targets the table's columns in declared order, or the first `n`
/// of them when its source supplies only `n`, as Postgres reads it. Filled in from the DDL, which is
/// authoritative about column order, and refused where the DDL may not give the table's real order:
/// one that alters the table (`ADD COLUMN` appends), drops a column, or was not read in full.
fn implicit_cols(mut p: Parts, schema: &Schema) -> Result<Parts, Refusal> {
    if !p.implicit_cols {
        return Ok(p);
    }
    let table = schema.table(&p.target).map_err(|e| unsupported(format!("schema: {e}")))?;
    if table.unread || table.altered || table.retried {
        return Err(unsupported(
            "INSERT without a column list, and the target's DDL is not read in full, so its column \
             order is not certain",
        ));
    }
    let n = match &p.src {
        Source::Values(rows) => rows.first().map_or(0, Vec::len),
        Source::Unnest { args, .. } => args.len(),
    };
    if n > table.columns.len() {
        return Err(Refusal::InvalidSql(format!(
            "INSERT has {n} expressions per row for the {} columns of {}",
            table.columns.len(),
            table.name
        )));
    }
    p.cols = table.columns[..n].iter().map(|c| c.name.clone()).collect();
    Ok(p)
}

fn lower(a: &Parts, b: &Parts, schema: &Schema) -> Result<Lowered, Refusal> {
    let (Source::Values(rows), Source::Unnest { args, .. }) = (&a.src, &b.src) else {
        unreachable!("oriented by translate")
    };
    // On the whole name, not the last part the schema is keyed by: `a.events` and `b.events` are two
    // tables, and so are `events` and `archive.events`, whichever of them the DDL declares.
    if a.target_path != b.target_path {
        return Err(unsupported("the two sides insert into different tables"));
    }
    if a.cols != b.cols {
        return Err(unsupported("the two sides' column lists differ"));
    }
    // As Postgres resolves their identifiers: `excluded."id"` and `EXCLUDED.id` are one name.
    if a.key != b.key {
        return Err(unsupported("the two sides' alias, conflict clause or RETURNING differ"));
    }
    let table = schema.table(&a.target).map_err(|e| unsupported(format!("schema: {e}")))?;
    if a.string_casts && schema.user_casts {
        return Err(unsupported(
            "a VALUES cell casts a parameter through a string type, and the DDL declares a cast of its \
             own, which could stand in for the target type's input",
        ));
    }
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

    let mut interner = Interner::new();
    let mut tys = Vec::with_capacity(k);
    let mut col_tys = Vec::with_capacity(k);
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
        col_tys.push(ty.name.as_str());
        col_ids.push(interner.id(format!("c:{c}")));
    }

    let mut lb_args = Vec::with_capacity(k);
    let mut widened = Vec::new();
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
        let e = interner.ty(&elem);
        if elem.array || !(e == tys[j] || WIDENS.contains(&(elem.name.as_str(), col_tys[j]))) {
            return Err(unsupported(format!(
                "unnest argument ${} is {}[], not the type of column {} or one that holds its every value",
                arg.param, arg.elem, a.cols[j]
            )));
        }
        if e != tys[j] {
            widened.push(j);
        }
        // B's own element type, so that the kernel checks it against the column's type itself.
        lb_args.push((arg.param, e));
    }

    // With a generated cell the pair is `checkGatherGen`'s, which numbers parameters by reading
    // order (`pinSeq`) instead of by slot: a generated cell takes a column but no number.
    let has_gen = rows.iter().flatten().any(|c| matches!(c, Cell::Gen(_)));
    if has_gen {
        pin_seq(rows)?;
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
                Cell::Gen(g) => {
                    out.push(match g.seq {
                        Some(Seq::Param(n)) => LCell::SeqP(n),
                        _ => LCell::Gen(g.kind),
                    });
                    continue;
                }
                Cell::Param(n, _) => *n,
            };
            if !has_gen && (n == 0 || (n as usize - 1) % k != j) {
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
                Cell::Null | Cell::Gen(_) => unreachable!(),
            }
        }
        la_rows.push(out);
    }
    if !has_gen {
        no_gaps(rows)?;
    }
    let generated = if has_gen { Some(generated_preconditions(a, rows, table)?) } else { None };

    let tail_a = tokens(&a.key, &mut interner);
    if tail_a.iter().any(|t| matches!(t, LTok::Param(_))) {
        return Err(unsupported("a parameter in the conflict clause or RETURNING"));
    }
    // Each side's target, columns and tail are interned from *that side's own* text, not copied from
    // A, so the kernel's equality checks compare two independent renderings. If the comparisons
    // above had a bug, the kernel would refuse the pair rather than prove it.
    let side = |p: &Parts, i: &mut Interner| {
        let target = i.id(format!("t:{}", p.target_path.join("\u{1f}")));
        let cols: Vec<u32> = p.cols.iter().map(|c| i.id(format!("c:{c}"))).collect();
        let tail = tokens(&p.key, i);
        (target, cols, tail)
    };
    let (ta, ca, tla) = side(a, &mut interner);
    let (tb, cb, tlb) = side(b, &mut interner);
    debug_assert_eq!(ca, col_ids);
    let la = LInsert { target: ta, cols: ca, src: LSource::Values(la_rows), tail: tla };
    let lb = LInsert { target: tb, cols: cb, src: LSource::Unnest(lb_args), tail: tlb };
    Ok(Lowered { tys, a: la, b: lb, shape: shape(rows), generated, widened })
}

struct Lowered {
    tys: Vec<u32>,
    a: LInsert,
    b: LInsert,
    shape: Shape,
    generated: Option<GenSummary>,
    widened: Vec<usize>,
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
        // Cast mismatch on either side: `smallint[]` cannot hold every `int` the VALUES side inserts.
        assert!(matches!(refused(v, "INSERT INTO t (a, b) SELECT * FROM unnest($1::smallint[], $2::text[])"), Refusal::Unsupported(_)));
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
        // A gap: pinned to their columns, but nothing uses $3 and $4.
        assert!(refused("INSERT INTO t (a, b) VALUES ($1, $2), ($5, $6)", B).reason().contains("$3 is never used"));
        // NULL cells leave gaps only if no other row fills them.
        assert!(pair("INSERT INTO t (a, b) VALUES ($1, NULL), ($3, $4), (NULL, $2)", B).is_ok());
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

    /// A table with one column per kind of default a `DEFAULT` cell can meet, and two side tables:
    /// one whose omitted column reads sequence state, one with a trigger.
    const GDDL: &str = "\
        CREATE TABLE g (id serial PRIMARY KEY, name text, ts timestamptz DEFAULT now(), \
          u uuid DEFAULT gen_random_uuid(), n int DEFAULT 0, \
          al int GENERATED ALWAYS AS IDENTITY, bd bigint GENERATED BY DEFAULT AS IDENTITY, \
          st int GENERATED ALWAYS AS (n + 1) STORED, s bigint DEFAULT nextval('g_s'));\
        CREATE TABLE h (id serial PRIMARY KEY, name text, last bigint DEFAULT currval('h_id_seq'));\
        CREATE TABLE tr (id serial PRIMARY KEY, name text, ts timestamptz);\
        CREATE TRIGGER t BEFORE INSERT ON tr FOR EACH ROW EXECUTE FUNCTION f();\
        CREATE TABLE al (id serial PRIMARY KEY, name text);\
        ALTER TABLE al ADD COLUMN extra int;\
        CREATE TABLE gc (id serial PRIMARY KEY, name text CHECK (char_length(name) > 0));\
        CREATE TABLE gk (id serial PRIMARY KEY, name text, ts timestamptz, k int DEFAULT pick());\
        CREATE TABLE q (id bigint PRIMARY KEY, name text, ts timestamptz DEFAULT now());\
        CREATE TABLE qs (id bigint PRIMARY KEY, name text, n serial);";

    fn gpair(a: &str, b: &str) -> Result<LeanPair, Refusal> {
        let p = |s: &str| Parser::parse_sql(&sqleq_frontend::internals::DIALECT, s).unwrap().remove(0);
        translate(&p(a), &p(b), &Schema::from_ddl(GDDL))
    }

    fn gen(p: &LeanPair) -> &GenSummary {
        p.generated.as_ref().expect("a generated pair")
    }

    const GB: &str = "INSERT INTO g (id, name) SELECT * FROM unnest($1::int[], $2::text[])";

    #[test]
    fn generated_cells_go_to_checkgathergen() {
        let p = gpair("INSERT INTO g (id, name) VALUES (DEFAULT, $1), (DEFAULT, $2)", GB).unwrap();
        let s = gen(&p);
        assert_eq!(s.funcs.get("default"), Some(&2));
        assert_eq!(s.columns, ["id"]);
        assert!(s.sequence && !s.mixed && !s.constant_default);
        assert_eq!(p.shape, Shape::RowMajor);
        let LSource::Values(rows) = &p.a.src else { panic!() };
        assert_eq!(rows[1], [LCell::Gen(GenKind::Default), LCell::P(2)]);

        // A column holding a generated cell in one row and a parameter in another.
        let p = gpair("INSERT INTO g (id, name) VALUES (DEFAULT, $1), ($2, $3)", GB).unwrap();
        assert!(gen(&p).mixed);

        // Generators with no sequence, and a constant default.
        let p = gpair(
            "INSERT INTO g (ts, u, n, name) VALUES (now(), gen_random_uuid(), DEFAULT, $1)",
            "INSERT INTO g (ts, u, n, name) SELECT * FROM unnest($1::timestamptz[], $2::uuid[], $3::int[], $4::text[])",
        )
        .unwrap();
        let s = gen(&p);
        assert!(!s.sequence && s.constant_default);
        assert_eq!(s.columns, ["ts", "u", "n"]);

        // `DEFAULT` on a recognised generator default, and on an identity BY DEFAULT.
        let p = gpair(
            "INSERT INTO g (ts, bd) VALUES (DEFAULT, DEFAULT)",
            "INSERT INTO g (ts, bd) SELECT * FROM unnest($1::timestamptz[], $2::bigint[])",
        )
        .unwrap();
        assert!(gen(&p).sequence);

        // Without a generated cell, the v1 path is untouched.
        assert!(gpair("INSERT INTO g (id, name) VALUES ($1, $2)", GB).unwrap().generated.is_none());
    }

    fn grefused(a: &str, b: &str) -> String {
        gpair(a, b).expect_err(&format!("{a} | {b} was not refused")).reason().to_string()
    }

    #[test]
    fn generated_cells_that_are_not_their_value_are_refused() {
        let bi = |cols: &str, tys: &str| format!("INSERT INTO g ({cols}) SELECT * FROM unnest({tys})");
        // A GENERATED ALWAYS identity, and a stored generated column.
        assert!(grefused("INSERT INTO g (al, name) VALUES (DEFAULT, $1)", &bi("al, name", "$1::int[], $2::text[]")).contains("GENERATED ALWAYS"));
        assert!(grefused("INSERT INTO g (st, name) VALUES (DEFAULT, $1)", &bi("st, name", "$1::int[], $2::text[]")).contains("GENERATED ALWAYS"));
        // A default the checker does not recognise.
        assert!(grefused("INSERT INTO gk (k, name) VALUES (DEFAULT, $1)", "INSERT INTO gk (k, name) SELECT * FROM unnest($1::int[], $2::text[])").contains("not a constant"));
        // A generator whose value the column cannot take.
        assert!(grefused("INSERT INTO g (n, name) VALUES (gen_random_uuid(), $1)", &bi("n, name", "$1::int[], $2::text[]")).contains("cannot fill"));
        assert!(grefused("INSERT INTO g (name, n) VALUES ($1, now())", &bi("name, n", "$1::text[], $2::int[]")).contains("cannot fill"));
        // Two generators in one column.
        assert!(grefused("INSERT INTO g (id, name) VALUES (DEFAULT, $1), (nextval('other'), $2)", GB).contains("more than one generator"));
        // Parameters out of reading order, a gap, and one used twice.
        assert!(grefused("INSERT INTO g (id, name) VALUES (DEFAULT, $2), (DEFAULT, $1)", GB).contains("reading order"));
        assert!(grefused("INSERT INTO g (id, name) VALUES (DEFAULT, $1), (DEFAULT, $3)", GB).contains("reading order"));
        assert!(grefused("INSERT INTO g (id, name) VALUES (DEFAULT, $1), (DEFAULT, $1)", GB).contains("reading order"));
    }

    #[test]
    fn a_generated_sequence_read_elsewhere_in_the_statement_is_refused() {
        // The tail reads sequence state, or a default.
        for tail in [" RETURNING currval('g_id_seq')", " ON CONFLICT (id) DO UPDATE SET n = DEFAULT"] {
            let r = grefused(&format!("INSERT INTO g (id, name) VALUES (DEFAULT, $1){tail}"), &format!("{GB}{tail}"));
            assert!(r.contains("sequence state or a default"), "{r}");
        }
        // The same tail is fine when no generated cell draws from a sequence.
        let tail = " RETURNING currval('g_id_seq')";
        assert!(gpair(
            &format!("INSERT INTO g (ts, name) VALUES (now(), $1){tail}"),
            &format!("INSERT INTO g (ts, name) SELECT * FROM unnest($1::timestamptz[], $2::text[]){tail}"),
        )
        .is_ok());
        // An omitted column whose default reads sequence state.
        let r = grefused(
            "INSERT INTO h (id, name) VALUES (DEFAULT, $1)",
            "INSERT INTO h (id, name) SELECT * FROM unnest($1::int[], $2::text[])",
        );
        assert!(r.contains("omitted column last"), "{r}");
        // An omitted column drawing from the sequence a generated cell uses.
        let r = grefused(
            "INSERT INTO g (id, name) VALUES (nextval('g_s'), $1)",
            GB,
        );
        assert!(r.contains("omitted column s"), "{r}");
        // An omitted column whose default is not recognised, which could call a function that draws
        // from the sequence; with no generated cell drawing from one, it is fine.
        let r = grefused(
            "INSERT INTO gk (id, name) VALUES (DEFAULT, $1)",
            "INSERT INTO gk (id, name) SELECT * FROM unnest($1::int[], $2::text[])",
        );
        assert!(r.contains("omitted column k is not recognised"), "{r}");
        assert!(gpair(
            "INSERT INTO gk (ts, name) VALUES (now(), $1)",
            "INSERT INTO gk (ts, name) SELECT * FROM unnest($1::timestamptz[], $2::text[])",
        )
        .is_ok());
        // A function called from RETURNING or from a CHECK, which could read the sequence; with no
        // generated cell drawing from one, the same RETURNING is fine.
        let r = grefused(
            "INSERT INTO g (id, name) VALUES (DEFAULT, $1) RETURNING lower(name)",
            &format!("{GB} RETURNING lower(name)"),
        );
        assert!(r.contains("RETURNING calls a function"), "{r}");
        assert!(gpair(
            "INSERT INTO g (ts, name) VALUES (now(), $1) RETURNING lower(name)",
            "INSERT INTO g (ts, name) SELECT * FROM unnest($1::timestamptz[], $2::text[]) RETURNING lower(name)",
        )
        .is_ok());
        let r = grefused(
            "INSERT INTO gc (id, name) VALUES (DEFAULT, $1)",
            "INSERT INTO gc (id, name) SELECT * FROM unnest($1::int[], $2::text[])",
        );
        assert!(r.contains("a CHECK on the target calls a function"), "{r}");
        // A trigger, which could read the sequence; and an altered table, whose DDL is not read whole.
        let r = grefused(
            "INSERT INTO tr (id, name) VALUES (DEFAULT, $1)",
            "INSERT INTO tr (id, name) SELECT * FROM unnest($1::int[], $2::text[])",
        );
        assert!(r.contains("trigger"), "{r}");
        let r = grefused(
            "INSERT INTO al (id, name) VALUES (DEFAULT, $1)",
            "INSERT INTO al (id, name) SELECT * FROM unnest($1::int[], $2::text[])",
        );
        assert!(r.contains("read in full"), "{r}");
    }

    const QB: &str = "INSERT INTO q (id, name) SELECT * FROM unnest($1::int8[], $2::text[])";

    #[test]
    fn a_sequence_named_by_a_parameter_takes_its_number_in_reading_order() {
        let p = gpair("INSERT INTO q (id, name) VALUES (nextval($1), $2), (nextval($3), $4)", QB).unwrap();
        let s = gen(&p);
        assert_eq!(s.funcs.get("nextval"), Some(&2));
        assert_eq!(s.columns, ["id"]);
        assert!(s.sequence);
        assert_eq!(p.shape, Shape::RowMajor);
        let LSource::Values(rows) = &p.a.src else { panic!() };
        assert_eq!(rows[1], [LCell::SeqP(3), LCell::P(4)]);
        // Numbered as if the name took no number, one name reused, and out of order.
        for a in [
            "INSERT INTO q (id, name) VALUES (nextval($1), $1), (nextval($2), $2)",
            "INSERT INTO q (id, name) VALUES (nextval($1), $2), (nextval($1), $3)",
            "INSERT INTO q (id, name) VALUES (nextval($2), $1)",
        ] {
            assert!(grefused(a, QB).contains("reading order"), "{a}");
        }
        // A named sequence in the same column is a second generator.
        assert!(grefused("INSERT INTO q (id, name) VALUES (nextval($1), $2), (nextval('s'), $3)", QB).contains("more than one generator"));
        // A column nextval's bigint cannot fill.
        let r = grefused(
            "INSERT INTO q (name, id) VALUES (nextval($1), $2)",
            "INSERT INTO q (name, id) SELECT * FROM unnest($1::text[], $2::int8[])",
        );
        assert!(r.contains("cannot fill"), "{r}");
    }

    #[test]
    fn a_sequence_named_by_a_parameter_could_be_any_sequence() {
        // An omitted column drawing from some sequence (a serial)...
        let r = grefused(
            "INSERT INTO qs (id, name) VALUES (nextval($1), $2)",
            "INSERT INTO qs (id, name) SELECT * FROM unnest($1::int8[], $2::text[])",
        );
        assert!(r.contains("could be any"), "{r}");
        // ...which a named sequence it does not draw from leaves alone.
        assert!(gpair(
            "INSERT INTO qs (id, name) VALUES (nextval('other'), $1)",
            "INSERT INTO qs (id, name) SELECT * FROM unnest($1::int8[], $2::text[])",
        )
        .is_ok());
        // An omitted column whose default is not recognised, refused as for any sequence.
        let r = grefused(
            "INSERT INTO gk (id, name) VALUES (nextval($1), $2)",
            "INSERT INTO gk (id, name) SELECT * FROM unnest($1::int[], $2::text[])",
        );
        assert!(r.contains("omitted column k is not recognised"), "{r}");
        // A tail that reads sequence state, as for a named sequence.
        let tail = " RETURNING currval('q_seq')";
        let r = grefused(&format!("INSERT INTO q (id, name) VALUES (nextval($1), $2){tail}"), &format!("{QB}{tail}"));
        assert!(r.contains("sequence state or a default"), "{r}");
    }

    #[test]
    fn an_insert_without_a_column_list_takes_the_tables_columns_in_order() {
        let ddl = "CREATE TABLE w (a text, b int, c int);\
            CREATE TABLE wa (a text, b int); ALTER TABLE wa ADD COLUMN c int;";
        let p = |s: &str| Parser::parse_sql(&sqleq_frontend::internals::DIALECT, s).unwrap().remove(0);
        let tr = |a: &str, b: &str| translate(&p(a), &p(b), &Schema::from_ddl(ddl));
        let w3 = "INSERT INTO w SELECT * FROM unnest($1::text[], $2::int[], $3::int[])";
        // Both sides without a list: every column, in declared order.
        let pair = tr("INSERT INTO w VALUES ($1, $2, $3), ($4, $5, $6)", w3).unwrap();
        assert_eq!(pair.replay.insert, ["a", "b", "c"]);
        // The first n columns when the source supplies n, against a side that lists them.
        let pair = tr("INSERT INTO w VALUES ($1), ($2)", "INSERT INTO w (a) SELECT * FROM unnest($1::text[])").unwrap();
        assert_eq!(pair.replay.insert, ["a"]);
        // A list other than the one the other side implies.
        let r = tr("INSERT INTO w VALUES ($1, $2)", "INSERT INTO w (b, a) SELECT * FROM unnest($1::int[], $2::text[])");
        assert!(r.expect_err("not refused").reason().contains("column lists differ"));
        // A table whose DDL alters it: its column order is not certain.
        let r = tr("INSERT INTO wa VALUES ($1, $2)", "INSERT INTO wa SELECT * FROM unnest($1::text[], $2::int[])");
        assert!(r.expect_err("not refused").reason().contains("column order is not certain"));
        // More expressions than the table has columns.
        let r = tr("INSERT INTO w VALUES ($1, $2, $3, $4)", "INSERT INTO w SELECT * FROM unnest($1::text[], $2::int[], $3::int[], $4::int[])");
        assert!(matches!(r, Err(Refusal::InvalidSql(_))), "{r:?}");
    }

    #[test]
    fn a_string_cast_cell_is_refused_where_the_ddl_declares_a_cast() {
        let p = |s: &str| Parser::parse_sql(&sqleq_frontend::internals::DIALECT, s).unwrap().remove(0);
        let a = "INSERT INTO u (id, n) VALUES (CAST($1::varchar AS uuid), $2), (CAST($3::varchar AS uuid), $4)";
        let b = "INSERT INTO u (id, n) SELECT * FROM unnest($1::uuid[], $2::int[])";
        let ddl = "CREATE TABLE u (id uuid PRIMARY KEY, n int);";
        assert!(translate(&p(a), &p(b), &Schema::from_ddl(ddl)).is_ok());
        let with_cast = format!("{ddl} CREATE CAST (varchar AS uuid) WITH INOUT;");
        let r = translate(&p(a), &p(b), &Schema::from_ddl(&with_cast));
        assert!(r.expect_err("not refused").reason().contains("declares a cast of its own"));
    }

    /// The arbiter the witness model gets for `INSERT INTO t (id, a) … <tail>` over `ddl`.
    fn conflict_of(ddl: &str, tail: &str) -> LConflict {
        let p = |s: &str| Parser::parse_sql(&sqleq_frontend::internals::DIALECT, s).unwrap().remove(0);
        let a = format!("INSERT INTO t (id, a) VALUES ($1, $2), ($3, $4){tail}");
        let b = format!("INSERT INTO t (id, a) SELECT * FROM unnest($1::int[], $2::int[]){tail}");
        match translate(&p(&a), &p(&b), &Schema::from_ddl(ddl)).unwrap().witness {
            Witness::Spec(s) => s.conflict,
            Witness::Skip(r) => panic!("witness skipped: {r}"),
        }
    }

    #[test]
    fn a_deferrable_constraint_is_never_an_arbiter() {
        let twins = "CREATE TABLE t (id int, a int, CONSTRAINT t_plain UNIQUE (id), CONSTRAINT t_def UNIQUE (id) DEFERRABLE);";
        // Postgres raises 55000 on each of these before inserting a row.
        for (ddl, tail) in [
            ("CREATE TABLE t (id int PRIMARY KEY DEFERRABLE, a int);", " ON CONFLICT (id) DO NOTHING"),
            ("CREATE TABLE t (id int PRIMARY KEY, a int UNIQUE DEFERRABLE);", " ON CONFLICT DO NOTHING"),
            (
                "CREATE TABLE t (id int, a int, CONSTRAINT t_pkey PRIMARY KEY (id) DEFERRABLE INITIALLY DEFERRED);",
                " ON CONFLICT ON CONSTRAINT t_pkey DO NOTHING",
            ),
            (
                "CREATE TABLE t (id int PRIMARY KEY DEFERRABLE INITIALLY IMMEDIATE, a int);",
                " ON CONFLICT (id) DO UPDATE SET a = EXCLUDED.a",
            ),
            // Inference meets the deferrable twin too, so a plain key beside it does not help.
            (twins, " ON CONFLICT (id) DO NOTHING"),
        ] {
            assert_eq!(conflict_of(ddl, tail), LConflict::NoArbiter, "{ddl}{tail}");
        }
        // But naming the plain twin works, a deferrable key on another column is no arbiter here,
        // and `INITIALLY IMMEDIATE` alone is not deferrable.
        assert_eq!(conflict_of(twins, " ON CONFLICT ON CONSTRAINT t_plain DO NOTHING"), LConflict::NothingOn(0));
        assert_eq!(
            conflict_of("CREATE TABLE t (id int PRIMARY KEY, a int UNIQUE DEFERRABLE);", " ON CONFLICT (id) DO NOTHING"),
            LConflict::NothingOn(0)
        );
        assert_eq!(
            conflict_of("CREATE TABLE t (id int PRIMARY KEY INITIALLY IMMEDIATE, a int);", " ON CONFLICT (id) DO NOTHING"),
            LConflict::NothingOn(0)
        );
    }

    #[test]
    fn element_types_that_hold_every_column_value_are_accepted() {
        // `bigint[]` into the `int` column a, `text[]` into the `varchar(10)` column c.
        let p = pair("INSERT INTO t (a, c) VALUES ($1, $2)", "INSERT INTO t (a, c) SELECT * FROM unnest($1::bigint[], $2::text[])")
            .unwrap();
        let LSource::Unnest(args) = &p.b.src else { panic!("not unnest") };
        // B carries its own element types, not the columns', so the kernel compares the two.
        assert_eq!(args.iter().map(|a| a.1).collect::<Vec<_>>(), [2, 3]);
        assert_eq!(p.tys, [1, 4]);
        assert!(pair("INSERT INTO t (a, b) VALUES ($1, $2)", "INSERT INTO t (a, b) SELECT * FROM unnest($1::int8[], $2::varchar[])").is_ok());
    }

    #[test]
    fn element_types_that_do_not_hold_every_column_value_are_refused() {
        for (cols, b) in [
            // `int[]` into the `bigint` column d: an `int8` overflows it.
            ("a, d", "unnest($1::int[], $2::int[])"),
            // Unrelated types.
            ("a, b", "unnest($1::text[], $2::text[])"),
            ("a, b", "unnest($1::int[], $2::int[])"),
            // `char(1)` pads and truncates; not in the list.
            ("a, b", "unnest($1::int[], $2::char[])"),
            // A modifier on the element is refused as before, even where the base type widens.
            ("a, c", "unnest($1::int[], $2::varchar(20)[])"),
        ] {
            let a = format!("INSERT INTO t ({cols}) VALUES ($1, $2)");
            let r = refused(&a, &format!("INSERT INTO t ({cols}) SELECT * FROM {b}"));
            assert!(matches!(r, Refusal::Unsupported(_)), "{b}: {r:?}");
        }
    }

    #[test]
    fn the_pinned_type_ids_and_widenings_are_the_kernels() {
        let mut i = Interner::new();
        for (n, t) in PINNED_TYPES.iter().enumerate() {
            assert_eq!(i.id(format!("ty:{t}")), n as u32, "{t}");
        }
        let check = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/../lean/Sqleq/Check.lean")).unwrap();
        let lean = |t: &str| format!("ty{}{}", t[..1].to_ascii_uppercase(), &t[1..]);
        for (n, t) in PINNED_TYPES.iter().enumerate() {
            let def = format!("def {} : Nat := {n}\n", lean(t));
            assert!(check.contains(&def), "Check.lean lacks `{}`", def.trim());
        }
        let list: Vec<String> = WIDENS.iter().map(|(e, c)| format!("({}, {})", lean(e), lean(c))).collect();
        let list = format!("[{}]", list.join(", "));
        assert!(check.contains(&list), "Check.lean's widens is not {list}");
    }

    #[test]
    fn tail_tokens_see_parameters_anywhere() {
        let mut i = Interner::new();
        let t = Tail { alias: None, on: Some("DO UPDATE SET x=$12,y".into()), returning: None };
        assert!(tokens(&t, &mut i).contains(&LTok::Param(12)));
    }

    /// The two targets are compared on their whole name. The schema is keyed by the last part, so
    /// `a.t` and `b.t` used to pass as one table, and the pair was proved though it writes two.
    mod qualified_targets {
        use super::*;

        const BV: &str = "SELECT * FROM unnest($1::int[], $2::text[])";

        fn on(ta: &str, tb: &str) -> Result<LeanPair, Refusal> {
            pair(&format!("INSERT INTO {ta} (a, b) VALUES ($1, $2), ($3, $4)"), &format!("INSERT INTO {tb} (a, b) {BV}"))
        }

        #[test]
        fn two_qualifiers_of_one_table_name_are_two_tables() {
            for (ta, tb) in [("a.t", "b.t"), ("t", "archive.t"), ("public.t", "t"), ("\"S\".t", "s.t")] {
                let r = on(ta, tb).expect_err(&format!("{ta} and {tb} were not refused"));
                assert!(r.reason().contains("different tables"), "{ta} | {tb}: {}", r.reason());
            }
        }

        /// Control: one name, however it is spelled, is one table.
        #[test]
        fn one_qualified_name_on_both_sides_is_one_table() {
            for (ta, tb) in [("a.t", "a.t"), ("A.T", "a.t"), ("\"a\".t", "a.\"t\"")] {
                assert!(on(ta, tb).is_ok(), "{ta} | {tb}");
            }
        }
    }
}
