// Unless explicitly stated otherwise all files in this repository are licensed under the
// Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/).
// Copyright 2026-Present Datadog, Inc.

//! `Relation`/`Expr` (from `sqleq_solver::ir`) to cvc5 terms, and the check itself.
//!
//! The shape follows `Cvc5AbstractTranslator.java`, hook for hook: the two semantics differ only
//! in which collection sort holds a table and which kind implements each operator
//! ([`Semantics`]), and in what duplicate removal means (`bag.setof`, or nothing). Where this
//! translation departs from the Java one, the departure is documented at the operator:
//!
//! * **Casts and unknown functions are uninterpreted**, never erased. Java drops every `CAST`,
//!   which equates `CAST(a AS REAL) / b` with `a / b`; here a cast is an uninterpreted function
//!   named by its source and target type, and so is every function the encoding does not
//!   interpret -- strict (NULL in, NULL out) where the frontend's naming guarantees it
//!   (`q_conv_*`, `q_arith_*`, `q_cast_*`, `QCASTn`, and the handful `sqleq-solver` lists), and
//!   otherwise a function over the nullable sorts, which assumes nothing about NULL. An
//!   uninterpreted symbol cannot license a false proof, because the real function is one of its
//!   interpretations; it can only cost a proof, or admit a model Postgres would not.
//! * **`NOT NULL` is a constraint, not a sort.** Java declares a table with the column sorts the
//!   schema gives and maps it back to nullable columns; here every column of every table is
//!   `Nullable`, and a `NOT NULL` column is asserted to hold no null
//!   (`(= (bag.count (tuple null) ((_ table.project i) T)) 0)`), the same shape the key
//!   constraints already take. It spares a `bag.map` at every scan.
//! * **Subqueries.** Calcite rewrites `EXISTS`/`IN` into joins before Java sees them; the IR keeps
//!   them, so a non-correlated `EXISTS`, `IN` (with SQL's three-valued reading) and scalar
//!   subquery are encoded here. A correlated one -- a column of an enclosing row read inside the
//!   subquery -- is refused (`correlated`), as `IrToRel` refuses it.
//! * **`AVG`** is `avg(sum, count)`, an uninterpreted function of the two, as in `sqleq-solver`:
//!   Postgres's `avg` over integers is a `numeric` with its own rounding, which `/` is not.
//!
//! ## What the encoding approximates
//!
//! The IR's `INTEGER`, `DATE`, `TIME` and `TIMESTAMP` are cvc5 integers (unbounded: no overflow,
//! no `infinity`), `REAL` is a cvc5 real (an exact rational: `2.0` and `2.00` are one value,
//! where Postgres prints them apart), `VARCHAR` is a cvc5 string compared by code point, and the
//! opaque `VARBINARY` and `INTERVAL` are uninterpreted sorts whose `=` is identity, which
//! Postgres's is not (`'1 day' = '24 hours'`). A proof holds under these readings; a model is a
//! database under them, and may use a value Postgres has not got (a 40-digit integer, say). The
//! harness therefore calls a `sat` answer a `model`, not a `counterexample`.

use std::collections::{BTreeSet, HashMap};
use std::fmt;

use cvc5::{Kind, Solver, Sort, Term, TermManager};
use sqleq_solver::ir::{AggCall, Expr, Input, JoinKind, Relation, Schema, SortKey, TranslateError, Type};

/// Bag semantics, SQL's own, where a row's multiplicity is how often it occurs; or set semantics,
/// an approximation under which `UNION ALL`, `EXCEPT ALL` and duplicates in general collapse, which
/// the solver finds much easier. See `Cvc5BagsTranslator` and `Cvc5SetsTranslator`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Semantics {
    Bags,
    Sets,
}

impl Semantics {
    pub fn parse(s: &str) -> Option<Semantics> {
        match s {
            "bags" | "bag" => Some(Semantics::Bags),
            "sets" | "set" => Some(Semantics::Sets),
            _ => None,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Semantics::Bags => "bags",
            Semantics::Sets => "sets",
        }
    }
}

/// How the problem is built and checked.
#[derive(Clone, Debug)]
pub struct Options {
    pub semantics: Semantics,
    /// cvc5's `tlimit-per`, in milliseconds: the budget of the one `check-sat`.
    pub timeout_ms: Option<u64>,
    /// Further `set-option`s, applied after the defaults, so they can override them:
    /// `("bags-deq-decompose", "false")`, say.
    pub cvc5_options: Vec<(String, String)>,
    /// `check-models`: have cvc5 verify every model against the assertions before it is reported.
    /// On by default, because a model is a claim the harness acts on.
    pub check_models: bool,
}

impl Default for Options {
    fn default() -> Options {
        Options { semantics: Semantics::Bags, timeout_ms: None, cvc5_options: Vec::new(), check_models: true }
    }
}

/// Why a plan was not encoded: the IR reader's refusal, or a construct this encoding has no faithful
/// term for. `Display` spells the first as `sqleq-solver` does (`correlated`, `unsupported-sort`,
/// `aggregate:ARRAY_AGG`), so the two axes' refusals compare row by row, and the second as
/// `unsupported:<what>`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Refusal {
    Ir(TranslateError),
    Unsupported(String),
}

impl fmt::Display for Refusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Refusal::Ir(e) => write!(f, "{e}"),
            Refusal::Unsupported(what) => write!(f, "unsupported:{what}"),
        }
    }
}

/// A row of the problem that did not get an answer: a refusal, or a cvc5 API call that failed (an
/// option this cvc5 has not got, a sort mismatch the encoding did not foresee, a model that failed
/// `check-models`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Failure {
    Refused(Refusal),
    Cvc5(String),
}

impl fmt::Display for Failure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Failure::Refused(r) => write!(f, "NOTRANS {r}"),
            Failure::Cvc5(e) => write!(f, "ERROR {e}"),
        }
    }
}

impl std::error::Error for Failure {}

impl From<TranslateError> for Failure {
    fn from(e: TranslateError) -> Failure {
        Failure::Refused(Refusal::Ir(e))
    }
}

impl From<cvc5::Error> for Failure {
    fn from(e: cvc5::Error) -> Failure {
        Failure::Cvc5(e.message().to_string())
    }
}

fn unsupported(what: impl Into<String>) -> Failure {
    Failure::Refused(Refusal::Unsupported(what.into()))
}

type R<T> = Result<T, Failure>;

/// One table of a model: its rows, each a tuple of column values (`None` is NULL) with the
/// multiplicity cvc5 gave it (always 1 under set semantics).
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize)]
pub struct TableModel {
    pub name: String,
    pub rows: Vec<(Vec<Option<String>>, u64)>,
}

/// The database a `sat` answer is: the tables the two queries read, as cvc5 valued them.
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize)]
pub struct Model {
    pub tables: Vec<TableModel>,
}

impl Model {
    /// One line, in the shape sqleq-fuzz prints a counterexample: `emp=[(1,NULL,'x'); (2,3,'y')×2]`.
    pub fn summary(&self) -> String {
        let table = |t: &TableModel| {
            let rows: Vec<String> = t
                .rows
                .iter()
                .map(|(cells, n)| {
                    let cells: Vec<String> = cells.iter().map(|c| c.clone().unwrap_or_else(|| "NULL".into())).collect();
                    let row = format!("({})", cells.join(","));
                    if *n == 1 { row } else { format!("{row}×{n}") }
                })
                .collect();
            format!("{}=[{}]", t.name, rows.join("; "))
        };
        self.tables.iter().map(table).collect::<Vec<_>>().join(" ")
    }
}

/// What cvc5 said of a pair it was asked about.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Outcome {
    /// `unsat`: no database the encoding admits distinguishes the two queries.
    Equivalent,
    /// `sat`: `model` is a database on which the two encodings differ. `exact` says whether the
    /// problem had no uninterpreted symbol in it -- no unknown function, cast or opaque sort -- so
    /// that the model is a database and nothing more. Under an uninterpreted symbol a model also
    /// fixes that symbol's values, which the real function may not take (`cast(1)` need not differ
    /// from `1`), and so is only a candidate until something runs the two queries on it.
    Different { model: Model, exact: bool },
    /// `unknown`, with cvc5's explanation (`TIMEOUT`, `INCOMPLETE`, ...).
    Unknown(String),
    /// The two queries return rows of different sorts, so there is no equality to check: a
    /// different number of columns, or a column that is a string on one side and an integer on
    /// the other. Not a refutation -- the IR's types are sometimes a guard rather than Postgres's
    /// (`SUM` over integers is REAL to it, a `CASE` over booleans and `NULL` INTEGER), and two
    /// queries so typed can agree on every database -- so no model is claimed; the note says how
    /// the shapes differ.
    Incomparable(String),
}

/// Encode `input` under `opts` and ask cvc5.
pub fn check(input: &Input, opts: &Options) -> R<Outcome> {
    let mut e = Encoder::new(opts)?;
    e.check(input)
}

/// The problem [`check`] would solve, as SMT-LIB text, for re-checking with the `cvc5` binary.
pub fn smt2(input: &Input, opts: &Options) -> R<String> {
    let mut e = Encoder::new(opts)?;
    e.problem(input)?;
    Ok(e.smt2())
}

/// The sort a value of an IR type has before it is made nullable.
fn kind_name(ty: Type) -> &'static str {
    ty.name()
}

/// A row in scope: the bound tuple variable, and the logical column its first field is.
#[derive(Clone)]
struct Row {
    var: Term,
    base: usize,
    /// Per column of `var`, whether it is never NULL (see [`never_null`]); shorter or empty where
    /// nothing is known.
    nonnull: Vec<bool>,
}

/// A translated relation: its collection term. Everything else about it -- its arity, its column
/// sorts -- is read off the term's sort.
struct Rel {
    term: Term,
}

struct Encoder {
    tm: TermManager,
    solver: Solver,
    sem: Semantics,
    /// Table constants by schema index, once scanned, with their names.
    tables: Vec<Option<(String, Term)>>,
    /// Uninterpreted functions and constants by name, and the uninterpreted sorts, as declared.
    funs: HashMap<String, Term>,
    sorts: HashMap<&'static str, Sort>,
    /// Every assertion but the final disequality, with the comment that explains it.
    constraints: Vec<(String, Term)>,
    /// The SMT-LIB preamble: logic and options, as set.
    prologue: Vec<String>,
    /// Declarations in order, as SMT-LIB text.
    declarations: Vec<String>,
    /// The bound variables, one per sort and role (see [`Encoder::var`]).
    vars: Vec<(Sort, &'static str, usize, Term)>,
    /// The two sides, once encoded, and the disequality asserted between them.
    sides: Option<(Term, Term)>,
    /// Whether no uninterpreted symbol has been declared: see [`Outcome::Different`].
    exact: bool,
    /// The rows of the queries enclosing the subquery being translated, innermost last: a column
    /// below the subquery's base is one of theirs (a correlated subquery).
    outer: Vec<Row>,
    /// How many subqueries enclose the one being translated: its variables are named apart from
    /// the enclosing rows', which its lambdas would otherwise capture.
    depth: usize,
}

const LOGIC: &str = "HO_ALL";

impl Encoder {
    fn new(opts: &Options) -> R<Encoder> {
        let tm = TermManager::new();
        let mut solver = Solver::new(&tm);
        let mut prologue = vec![format!("(set-logic {LOGIC})")];
        solver.set_logic(LOGIC)?;
        let mut options: Vec<(String, String)> = vec![
            ("produce-models".into(), "true".into()),
            ("check-models".into(), opts.check_models.to_string()),
            // The settings the Java translator has always run with.
            ("uf-lazy-ll".into(), "true".into()),
            ("fmf-bound".into(), "true".into()),
            ("strings-exp".into(), "true".into()),
        ];
        if let Some(ms) = opts.timeout_ms {
            options.push(("tlimit-per".into(), ms.to_string()));
        }
        options.extend(opts.cvc5_options.iter().cloned());
        for (k, v) in &options {
            solver.set_option(k, v)?;
            prologue.push(format!("(set-option :{k} {v})"));
        }
        Ok(Encoder {
            tm,
            solver,
            sem: opts.semantics,
            tables: Vec::new(),
            funs: HashMap::new(),
            sorts: HashMap::new(),
            constraints: Vec::new(),
            prologue,
            declarations: Vec::new(),
            vars: Vec::new(),
            sides: None,
            exact: true,
            outer: Vec::new(),
            depth: 0,
        })
    }

    // ── The problem ───────────────────────────────────────────────────────────────────────────────

    /// Encode both sides and assert that they differ, or record that no assertion is needed because
    /// their shapes already differ. Returns that note, when the two shapes differ.
    fn problem(&mut self, input: &Input) -> R<Option<String>> {
        self.tables = vec![None; input.schemas.len()];
        let q1 = self.rel(&input.queries[0], 0, &input.schemas)?;
        let q2 = self.rel(&input.queries[1], 0, &input.schemas)?;
        let (q1, q2) =
            self.coerce_null_columns(q1.term, &input.queries[0], q2.term, &input.queries[1], &input.schemas)?;
        let (q1, q2) = self.widen(q1, q2)?;
        let (s1, s2) = (q1.sort(), q2.sort());
        if s1 != s2 {
            let (w1, w2) = (self.width(&s1)?, self.width(&s2)?);
            return Ok(Some(if w1 != w2 {
                format!("the two queries return {w1} and {w2} columns")
            } else {
                format!("the two queries' columns differ in sort: {} against {}", self.elem(&s1)?, self.elem(&s2)?)
            }));
        }
        let q1 = Rel { term: q1 };
        let q2 = Rel { term: q2 };
        let eq = self.tm.mk_term(Kind::Equal, &[q1.term.clone(), q2.term.clone()])?;
        let differ = self.tm.mk_term(Kind::Not, &[eq])?;
        for (_, c) in &self.constraints {
            self.solver.assert_formula(c.clone())?;
        }
        self.solver.assert_formula(differ)?;
        self.sides = Some((q1.term, q2.term));
        Ok(None)
    }

    fn check(&mut self, input: &Input) -> R<Outcome> {
        if let Some(note) = self.problem(input)? {
            return Ok(Outcome::Incomparable(note));
        }
        let r = self.solver.check_sat()?;
        if r.is_unsat() {
            return Ok(Outcome::Equivalent);
        }
        if r.is_sat() {
            let model = self.model()?;
            return Ok(Outcome::Different { model, exact: self.exact });
        }
        Ok(Outcome::Unknown(cvc5::unknown_explanation_to_string(r.unknown_explanation())))
    }

    /// The problem as SMT-LIB: the preamble, every declaration, every assertion with its comment.
    fn smt2(&self) -> String {
        let mut out = self.prologue.clone();
        out.extend(self.declarations.iter().cloned());
        for (comment, c) in &self.constraints {
            out.push(format!("; {comment}"));
            out.push(format!("(assert {c})"));
        }
        match &self.sides {
            Some((a, b)) => {
                out.push("; the two queries differ".into());
                out.push(format!("(assert (not (= {a} {b})))"));
            }
            None => out.push("; the two queries return rows of different shapes".into()),
        }
        out.push("(check-sat)".into());
        out.push("(get-model)".into());
        out.join("\n") + "\n"
    }

    /// The two sides over one row sort where one side's column is NULL in every row and the other's
    /// is of another sort: that side's column is read as the other sort's NULL. Calcite writes an
    /// empty relation of any type as `SELECT NULL AS a, .. WHERE 1 = 0`, whose NULLs the IR types as
    /// integers, and a NULL of one type and a NULL of another are the same cell to a comparison of
    /// rows. Columns differing any other way are left for [`Self::widen`] or the caller.
    fn coerce_null_columns(
        &mut self,
        a: Term,
        ra: &Relation,
        b: Term,
        rb: &Relation,
        schemas: &[Schema],
    ) -> R<(Term, Term)> {
        let (sa, sb) = (self.column_sorts(&a)?, self.column_sorts(&b)?);
        if sa.len() != sb.len() || sa == sb {
            return Ok((a, b));
        }
        let mut fix_a: Vec<(usize, Sort)> = Vec::new();
        let mut fix_b: Vec<(usize, Sort)> = Vec::new();
        for (i, (x, y)) in sa.iter().zip(&sb).enumerate() {
            if x == y {
                continue;
            }
            if always_null(ra, i, schemas) {
                fix_a.push((i, y.clone()));
            } else if always_null(rb, i, schemas) {
                fix_b.push((i, x.clone()));
            }
        }
        let a = self.null_columns(a, &fix_a)?;
        let b = self.null_columns(b, &fix_b)?;
        Ok((a, b))
    }

    /// `table` with each of `columns` replaced by the NULL of the sort given for it: for column 1
    /// read as a string,
    /// `(bag.map (lambda ((t T)) (tuple ((_ tuple.select 0) t) (as nullable.null (Nullable String))))
    /// table)`.
    fn null_columns(&mut self, table: Term, columns: &[(usize, Sort)]) -> R<Term> {
        if columns.is_empty() {
            return Ok(table);
        }
        let elem = self.elem_of(&table)?;
        let n = elem.tuple_length()?;
        let t = self.var(elem, "t")?;
        let mut cells = Vec::with_capacity(n);
        for i in 0..n {
            match columns.iter().find(|(c, _)| *c == i) {
                Some((_, sort)) => cells.push(self.null(sort.clone())?),
                None => cells.push(self.select(t.clone(), i)?),
            }
        }
        let body = self.tm.mk_tuple(&cells)?;
        let f = self.lambda(&[t], body)?;
        Ok(self.tm.mk_term(self.map_kind(), &[f, table])?)
    }

    /// The two sides over one row sort where they differ only in an integer column against a real
    /// one: the integer side is read as reals there (`to_real`, under a map), the way Postgres
    /// compares a `bigint` with a `numeric`. Any other difference is left for the caller to report.
    fn widen(&mut self, a: Term, b: Term) -> R<(Term, Term)> {
        let (sa, sb) = (self.column_sorts(&a)?, self.column_sorts(&b)?);
        if sa.len() != sb.len() || sa == sb {
            return Ok((a, b));
        }
        let real = self.col_sort(Type::Real)?;
        let int = self.col_sort(Type::Integer)?;
        let mut widen_a = Vec::new();
        let mut widen_b = Vec::new();
        for (i, (x, y)) in sa.iter().zip(&sb).enumerate() {
            if x == y {
                continue;
            }
            match (x == &int && y == &real, x == &real && y == &int) {
                (true, _) => widen_a.push(i),
                (_, true) => widen_b.push(i),
                _ => return Ok((a, b)),
            }
        }
        let a = self.widen_columns(a, &widen_a)?;
        let b = self.widen_columns(b, &widen_b)?;
        Ok((a, b))
    }

    fn widen_columns(&mut self, table: Term, columns: &[usize]) -> R<Term> {
        if columns.is_empty() {
            return Ok(table);
        }
        let elem = self.elem_of(&table)?;
        let n = elem.tuple_length()?;
        let t = self.var(elem, "t")?;
        let mut cells = Vec::with_capacity(n);
        for i in 0..n {
            let c = self.select(t.clone(), i)?;
            cells.push(if columns.contains(&i) { self.lift(Kind::ToReal, &[c])? } else { c });
        }
        let body = self.tm.mk_tuple(&cells)?;
        let f = self.lambda(&[t], body)?;
        Ok(self.tm.mk_term(self.map_kind(), &[f, table])?)
    }

    // ── Sorts ─────────────────────────────────────────────────────────────────────────────────────

    /// The sort of a non-null value of `ty`.
    fn base_sort(&mut self, ty: Type) -> Sort {
        match ty {
            Type::Integer | Type::Date | Type::Time | Type::Timestamp => self.tm.integer_sort(),
            Type::Real => self.tm.real_sort(),
            Type::Boolean => self.tm.boolean_sort(),
            Type::Varchar => self.tm.string_sort(),
            Type::Varbinary => self.uninterpreted_sort("Opaque"),
            Type::Interval => self.uninterpreted_sort("Interval"),
        }
    }

    fn uninterpreted_sort(&mut self, name: &'static str) -> Sort {
        if let Some(s) = self.sorts.get(name) {
            return s.clone();
        }
        let s = self.tm.mk_uninterpreted_sort(name);
        self.exact = false;
        self.declarations.push(format!("(declare-sort {name} 0)"));
        self.sorts.insert(name, s.clone());
        s
    }

    /// The sort of a column of `ty`: nullable, whatever the schema says (see the module doc).
    fn col_sort(&mut self, ty: Type) -> R<Sort> {
        let base = self.base_sort(ty);
        Ok(self.tm.mk_nullable_sort(base)?)
    }

    fn row_sort(&mut self, types: &[Type]) -> R<Sort> {
        let sorts: Vec<Sort> = types.iter().map(|t| self.col_sort(*t)).collect::<R<_>>()?;
        Ok(self.tm.mk_tuple_sort(&sorts)?)
    }

    fn table_sort(&mut self, row: Sort) -> R<Sort> {
        Ok(match self.sem {
            Semantics::Bags => self.tm.mk_bag_sort(row)?,
            Semantics::Sets => self.tm.mk_set_sort(row)?,
        })
    }

    /// The tuple sort a collection sort holds.
    fn elem(&self, table: &Sort) -> R<Sort> {
        Ok(match self.sem {
            Semantics::Bags => table.bag_element_sort()?,
            Semantics::Sets => table.set_element_sort()?,
        })
    }

    fn width(&self, table: &Sort) -> R<usize> {
        Ok(self.elem(table)?.tuple_length()?)
    }

    fn elem_of(&self, t: &Term) -> R<Sort> {
        self.elem(&t.sort())
    }

    fn column_sorts(&self, t: &Term) -> R<Vec<Sort>> {
        Ok(self.elem_of(t)?.tuple_element_sorts()?)
    }

    // ── The semantics' hooks (Cvc5BagsTranslator / Cvc5SetsTranslator) ──────────────────────────

    fn project_kind(&self) -> Kind {
        match self.sem {
            Semantics::Bags => Kind::TableProject,
            Semantics::Sets => Kind::RelationProject,
        }
    }

    fn product_kind(&self) -> Kind {
        match self.sem {
            Semantics::Bags => Kind::TableProduct,
            Semantics::Sets => Kind::RelationProduct,
        }
    }

    fn aggregate_kind(&self) -> Kind {
        match self.sem {
            Semantics::Bags => Kind::TableAggregate,
            Semantics::Sets => Kind::RelationAggregate,
        }
    }

    fn filter_kind(&self) -> Kind {
        match self.sem {
            Semantics::Bags => Kind::BagFilter,
            Semantics::Sets => Kind::SetFilter,
        }
    }

    fn map_kind(&self) -> Kind {
        match self.sem {
            Semantics::Bags => Kind::BagMap,
            Semantics::Sets => Kind::SetMap,
        }
    }

    /// Multiset sum: `UNION ALL`, literal `VALUES`, outer-join padding.
    fn union_all_kind(&self) -> Kind {
        match self.sem {
            Semantics::Bags => Kind::BagUnionDisjoint,
            Semantics::Sets => Kind::SetUnion,
        }
    }

    fn intersection_kind(&self) -> Kind {
        match self.sem {
            Semantics::Bags => Kind::BagInterMin,
            Semantics::Sets => Kind::SetInter,
        }
    }

    /// Difference that drops a row entirely once it matches: the unmatched rows of an outer join,
    /// and `EXCEPT`.
    fn difference_remove_kind(&self) -> Kind {
        match self.sem {
            Semantics::Bags => Kind::BagDifferenceRemove,
            Semantics::Sets => Kind::SetMinus,
        }
    }

    fn empty_table(&mut self, table_sort: Sort) -> R<Term> {
        Ok(match self.sem {
            Semantics::Bags => self.tm.mk_empty_bag(table_sort)?,
            Semantics::Sets => self.tm.mk_empty_set(table_sort)?,
        })
    }

    fn singleton(&mut self, tuple: Term) -> R<Term> {
        Ok(match self.sem {
            Semantics::Bags => {
                let one = self.tm.mk_integer(1);
                self.tm.mk_term(Kind::BagMake, &[tuple, one])?
            }
            Semantics::Sets => self.tm.mk_term(Kind::SetSingleton, &[tuple])?,
        })
    }

    /// Duplicate removal: `bag.setof` caps every multiplicity at one; a set has none to cap.
    fn distinct(&mut self, table: Term) -> R<Term> {
        Ok(match self.sem {
            Semantics::Bags => self.tm.mk_term(Kind::BagSetof, &[table])?,
            Semantics::Sets => table,
        })
    }

    fn is_empty(&mut self, table: Term) -> R<Term> {
        let empty = self.empty_table(table.sort())?;
        Ok(self.tm.mk_term(Kind::Equal, &[table, empty])?)
    }

    fn member(&mut self, tuple: Term, table: Term) -> R<Term> {
        Ok(match self.sem {
            Semantics::Bags => self.tm.mk_term(Kind::BagMember, &[tuple, table])?,
            Semantics::Sets => self.tm.mk_term(Kind::SetMember, &[tuple, table])?,
        })
    }

    fn card(&mut self, table: Term) -> R<Term> {
        Ok(match self.sem {
            Semantics::Bags => self.tm.mk_term(Kind::BagCard, &[table])?,
            Semantics::Sets => self.tm.mk_term(Kind::SetCard, &[table])?,
        })
    }

    fn choose(&mut self, table: Term) -> R<Term> {
        Ok(match self.sem {
            Semantics::Bags => self.tm.mk_term(Kind::BagChoose, &[table])?,
            Semantics::Sets => self.tm.mk_term(Kind::SetChoose, &[table])?,
        })
    }

    fn project(&mut self, indices: &[u32], table: Term) -> R<Term> {
        let op = self.tm.mk_op(self.project_kind(), indices)?;
        Ok(self.tm.mk_term_from_op(op, &[table])?)
    }

    // ── Terms ─────────────────────────────────────────────────────────────────────────────────────

    /// The bound variable of `sort` in the role `prefix` names -- `t` for a row, `x` and `y` for a
    /// fold's row and accumulator, `r` for a subquery's row -- the same one every time. No lambda
    /// body here reads an enclosing lambda's variable (a correlated subquery, which would, is
    /// refused), so a nested lambda over the same sort shadows harmlessly, and two lambdas that
    /// compute the same thing are one term: the two sides of a pair share most of their plan, and
    /// cvc5 does not identify lambdas up to the names of their variables on its own.
    fn var(&mut self, sort: Sort, prefix: &'static str) -> R<Term> {
        let depth = self.depth;
        if let Some((_, _, _, v)) = self.vars.iter().find(|(s, p, d, _)| *s == sort && *p == prefix && *d == depth) {
            return Ok(v.clone());
        }
        // `t__0`: not a table's name (a table `t` and a row variable `t` would be one symbol to a
        // reader of the printed problem, which binds by name), and apart per nesting level
        let name = format!("{prefix}__{depth}");
        let v = self.tm.mk_var(sort.clone(), &name)?;
        self.vars.push((sort, prefix, depth, v.clone()));
        Ok(v)
    }

    /// `(lambda vars body)`. The body is not rewritten here: cvc5 rewrites the assertions, lambda
    /// bodies included, after its bags-facts pass has read the keys and NOT NULL columns off the
    /// constraints, and the rewriter keeps what it rewrote, so a body rewritten before would keep a
    /// form that does not use them.
    fn lambda(&mut self, vars: &[Term], body: Term) -> R<Term> {
        let list = self.tm.mk_term(Kind::VariableList, vars)?;
        Ok(self.tm.mk_term(Kind::Lambda, &[list, body])?)
    }

    fn select(&mut self, tuple: Term, index: usize) -> R<Term> {
        let sort = tuple.sort();
        let dt = sort.datatype()?;
        let sel = dt.constructor(0)?.selector(index)?.term();
        Ok(self.tm.mk_term(Kind::ApplySelector, &[sel, tuple])?)
    }

    fn some(&mut self, t: Term) -> R<Term> {
        Ok(self.tm.mk_nullable_some(t)?)
    }

    fn val(&mut self, t: Term) -> R<Term> {
        Ok(self.tm.mk_nullable_val(t)?)
    }

    fn is_null(&mut self, t: Term) -> R<Term> {
        Ok(self.tm.mk_nullable_is_null(t)?)
    }

    fn is_some(&mut self, t: Term) -> R<Term> {
        Ok(self.tm.mk_nullable_is_some(t)?)
    }

    fn null(&mut self, nullable_sort: Sort) -> R<Term> {
        Ok(self.tm.mk_nullable_null(nullable_sort)?)
    }

    fn ite(&mut self, c: Term, a: Term, b: Term) -> R<Term> {
        Ok(self.tm.mk_term(Kind::Ite, &[c, a, b])?)
    }

    fn and(&mut self, ts: Vec<Term>) -> R<Term> {
        Ok(match ts.len() {
            0 => self.tm.mk_true(),
            1 => ts.into_iter().next().expect("one"),
            _ => self.tm.mk_term(Kind::And, &ts)?,
        })
    }

    fn or(&mut self, ts: Vec<Term>) -> R<Term> {
        Ok(match ts.len() {
            0 => self.tm.mk_false(),
            1 => ts.into_iter().next().expect("one"),
            _ => self.tm.mk_term(Kind::Or, &ts)?,
        })
    }

    fn not(&mut self, t: Term) -> R<Term> {
        Ok(self.tm.mk_term(Kind::Not, &[t])?)
    }

    /// A nullable boolean read two-valued, as a `WHERE` reads it: only `some(true)` passes.
    fn truth(&mut self, t: Term) -> R<Term> {
        let s = self.is_some(t.clone())?;
        let v = self.val(t)?;
        self.and(vec![s, v])
    }

    /// A nullable boolean that is `some(false)`: what `NOT` turns into a pass,
    /// `(and (nullable.is_some t) (not (nullable.val t)))`.
    fn falsity(&mut self, t: Term) -> R<Term> {
        let s = self.is_some(t.clone())?;
        let v = self.val(t)?;
        let nv = self.not(v)?;
        self.and(vec![s, nv])
    }

    /// The two-valued reading of a condition, with SQL's `AND`, `OR` and `NOT` read as the Boolean
    /// connectives over the truth values of their operands: `a AND b` passes exactly when both do,
    /// `a OR b` when one does, `NOT a` when `a` is false. This is the same predicate as the truth
    /// of the three-valued `ite` encoding, but it leaves a conjunction visible as a conjunction,
    /// which lets the solver's rewriter move one conjunct at a time, such as a `HAVING` condition
    /// on a grouping column below the aggregate. For `a = b AND c`,
    /// `(and (nullable.is_some a) (= a b) (nullable.is_some c) (nullable.val c))`, up to the
    /// flattening of `and`.
    fn condition(&mut self, e: &Expr, row: &Row, schemas: &[Schema]) -> R<Term> {
        match e {
            Expr::Call { operator, operand, .. } if operator == "AND" => {
                let ts = operand.iter().map(|a| self.condition(a, row, schemas)).collect::<R<Vec<_>>>()?;
                self.and(ts)
            }
            Expr::Call { operator, operand, .. } if operator == "OR" => {
                let ts = operand.iter().map(|a| self.condition(a, row, schemas)).collect::<R<Vec<_>>>()?;
                self.or(ts)
            }
            Expr::Call { operator, operand, .. } if operator == "NOT" && operand.len() == 1 => {
                self.negated_condition(&operand[0], row, schemas)
            }
            Expr::Call { operator, operand, .. } if operator == "=" && operand.len() == 2 => {
                // `a = b` holds exactly when `a` is not NULL and equals `b`: the truth of the
                // three-valued comparison, without the `ite` over NULLs, so a join on a column is an
                // equation between the two columns the solver's rewriter can read.
                let a = self.expr(&operand[0], row, schemas)?;
                let b = self.expr(&operand[1], row, schemas)?;
                let (a, b) = self.unify("=", a, b)?;
                let some = self.is_some(a.clone())?;
                let eq = self.tm.mk_term(Kind::Equal, &[a, b])?;
                self.and(vec![some, eq])
            }
            _ => {
                let t = self.expr(e, row, schemas)?;
                self.truth(t)
            }
        }
    }

    /// The two-valued reading of `NOT e`: `e` is false, with De Morgan over `AND` and `OR`; for
    /// `NOT (a OR b)`, `(and (nullable.is_some a) (not (nullable.val a)) (nullable.is_some b)
    /// (not (nullable.val b)))`.
    fn negated_condition(&mut self, e: &Expr, row: &Row, schemas: &[Schema]) -> R<Term> {
        match e {
            Expr::Call { operator, operand, .. } if operator == "AND" => {
                let ts =
                    operand.iter().map(|a| self.negated_condition(a, row, schemas)).collect::<R<Vec<_>>>()?;
                self.or(ts)
            }
            Expr::Call { operator, operand, .. } if operator == "OR" => {
                let ts =
                    operand.iter().map(|a| self.negated_condition(a, row, schemas)).collect::<R<Vec<_>>>()?;
                self.and(ts)
            }
            Expr::Call { operator, operand, .. } if operator == "NOT" && operand.len() == 1 => {
                self.condition(&operand[0], row, schemas)
            }
            _ => {
                let t = self.expr(e, row, schemas)?;
                self.falsity(t)
            }
        }
    }

    /// `op` over the values of nullable `args`, null when any of them is. Not cvc5's own
    /// `nullable.lift`: that wraps `op` in a lambda with fresh bound variables every time, so two
    /// occurrences of one expression are two functions to the bags solver, which relates maps and
    /// filters by the identity of their functions; two sides that compute `a + 5` the same way
    /// then stop being one term. Spelled out as an `ite`, the two are identical.
    fn lift(&mut self, op: Kind, args: &[Term]) -> R<Term> {
        self.strict(args, |enc, vals| Ok(enc.tm.mk_term(op, &vals)?))
    }

    /// `body(values)` over the values of nullable `args`, null when any of them is, for a body
    /// `nullable.lift` cannot express with one kind.
    fn strict(&mut self, args: &[Term], body: impl FnOnce(&mut Encoder, Vec<Term>) -> R<Term>) -> R<Term> {
        let nulls: Vec<Term> = args.iter().map(|a| self.is_null(a.clone())).collect::<R<_>>()?;
        let vals: Vec<Term> = args.iter().map(|a| self.val(a.clone())).collect::<R<_>>()?;
        let value = body(self, vals)?;
        let some = self.some(value)?;
        let null = self.null(some.sort())?;
        let any_null = self.or(nulls)?;
        self.ite(any_null, null, some)
    }

    /// An uninterpreted function `name`, declared on first use.
    fn fun(&mut self, name: &str, domain: &[Sort], codomain: Sort) -> R<Term> {
        if let Some(f) = self.funs.get(name) {
            return Ok(f.clone());
        }
        self.exact = false;
        let f = if domain.is_empty() {
            self.tm.mk_const(codomain.clone(), name)?
        } else {
            self.solver.declare_fun(name, domain, codomain.clone())?
        };
        let args: Vec<String> = domain.iter().map(|s| s.to_string()).collect();
        self.declarations.push(if domain.is_empty() {
            format!("(declare-const {f} {codomain})")
        } else {
            format!("(declare-fun {f} ({}) {codomain})", args.join(" "))
        });
        self.funs.insert(name.to_string(), f.clone());
        Ok(f)
    }

    fn apply(&mut self, f: Term, args: &[Term]) -> R<Term> {
        if args.is_empty() {
            return Ok(f);
        }
        let mut children = vec![f];
        children.extend(args.iter().cloned());
        Ok(self.tm.mk_term(Kind::ApplyUf, &children)?)
    }

    // ── Relations (Cvc5AbstractTranslator.translate(RelNode)) ───────────────────────────────────

    /// `r` as a collection of tuples, its columns numbered from `base` for the expressions over it.
    fn rel(&mut self, r: &Relation, base: usize, schemas: &[Schema]) -> R<Rel> {
        match r {
            Relation::Scan(i) => self.scan(*i, schemas),
            Relation::Distinct(source) => {
                let src = self.rel(source, base, schemas)?;
                Ok(Rel { term: self.distinct(src.term)? })
            }
            Relation::Filter { source, condition } => {
                let src = self.rel(source, base, schemas)?;
                let nonnull = never_null_columns(source, base, schemas);
                self.filter(condition, src.term, base, nonnull, schemas)
            }
            Relation::Project { source, target } => self.projection(source, target, base, schemas),
            Relation::Join { left, right, kind, condition } => self.join(left, right, *kind, condition, base, schemas),
            Relation::Group { source, keys, function } => self.group(source, keys, function, base, schemas),
            // Bare ORDER BY changes no multiplicity; a row slice does, and has no encoding here.
            Relation::Sort { source, offset, limit, keys } => {
                // `LIMIT 0` (or `FETCH NEXT 0 ROWS`) keeps no row, whatever the order
                if matches!(limit, Some(Expr::Literal { value, .. }) if value.trim() == "0") {
                    let src = self.rel(source, base, schemas)?;
                    let sort = src.term.sort();
                    return Ok(Rel { term: self.empty_table(sort)? });
                }
                if offset.is_some() || limit.is_some() {
                    return self.row_slice(source, offset.as_ref(), limit.as_ref(), keys, base, schemas);
                }
                self.rel(source, base, schemas)
            }
            Relation::Values { schema, content } => self.values(schema, content, base, schemas),
            Relation::Union(sides) => {
                let (l, r) = self.sides(sides, base, schemas)?;
                Ok(Rel { term: self.tm.mk_term(self.union_all_kind(), &[l, r])? })
            }
            // `INTERSECT` and `EXCEPT` (never `ALL`: the frontend refuses those) return distinct rows.
            Relation::Intersect(sides) => {
                let (l, r) = self.sides(sides, base, schemas)?;
                let both = self.tm.mk_term(self.intersection_kind(), &[l, r])?;
                Ok(Rel { term: self.distinct(both)? })
            }
            Relation::Except(sides) => {
                let (l, r) = self.sides(sides, base, schemas)?;
                let rest = self.tm.mk_term(self.difference_remove_kind(), &[l, r])?;
                Ok(Rel { term: self.distinct(rest)? })
            }
            // `INTERSECT ALL` keeps the fewer copies of a row, `EXCEPT ALL` the copies the first
            // branch has beyond the second's: multiplicities, which only bags have.
            Relation::IntersectAll(sides) => {
                if self.sem != Semantics::Bags {
                    return Err(unsupported("INTERSECT ALL under set semantics"));
                }
                let (l, r) = self.sides(sides, base, schemas)?;
                Ok(Rel { term: self.tm.mk_term(Kind::BagInterMin, &[l, r])? })
            }
            Relation::ExceptAll(sides) => {
                if self.sem != Semantics::Bags {
                    return Err(unsupported("EXCEPT ALL under set semantics"));
                }
                let (l, r) = self.sides(sides, base, schemas)?;
                Ok(Rel { term: self.tm.mk_term(Kind::BagDifferenceSubtract, &[l, r])? })
            }
        }
    }

    /// A row slice, `OFFSET o LIMIT l` under an `ORDER BY` whose keys contain a key of the rows: no
    /// two rows tie, so every row has its place, the number of rows sorted before it, and the slice
    /// keeps the rows placed in [o, o + l). Under ties the slice would depend on the order the rows
    /// come in, and is refused; so is a slice of a computed size, and one under set semantics. For
    /// `ORDER BY k LIMIT 2` over `rows`, k column 0, ascending and NULLs last,
    /// `(bag.filter (lambda ((x T)) (and (>= (bag.fold (lambda ((z T) (a Int)) (+ a 1)) 0 E) 0)
    /// (< (bag.fold (lambda ((z T) (a Int)) (+ a 1)) 0 E) 2))) rows)` with `E`, the rows before x,
    /// `(bag.filter (lambda ((y T)) (ite (nullable.is_null ((_ tuple.select 0) y)) false (ite
    /// (nullable.is_null ((_ tuple.select 0) x)) true (< (nullable.val ((_ tuple.select 0) y))
    /// (nullable.val ((_ tuple.select 0) x)))))) rows)`.
    fn row_slice(
        &mut self,
        source: &Relation,
        offset: Option<&Expr>,
        limit: Option<&Expr>,
        keys: &[SortKey],
        base: usize,
        schemas: &[Schema],
    ) -> R<Rel> {
        let constant = |e: Option<&Expr>| -> R<Option<i64>> {
            match e {
                None => Ok(None),
                Some(Expr::Literal { value, .. }) => {
                    value.trim().parse::<i64>().map(Some).map_err(|_| TranslateError::UnsupportedSort.into())
                }
                Some(_) => Err(TranslateError::UnsupportedSort.into()),
            }
        };
        let off = constant(offset)?.unwrap_or(0);
        let lim = constant(limit)?;
        let sorted: BTreeSet<usize> = keys.iter().map(|k| k.column as usize).collect();
        let unique = relation_keys(source, base, schemas).iter().any(|k| k.iter().all(|c| sorted.contains(c)));
        if self.sem != Semantics::Bags || !unique {
            return Err(TranslateError::UnsupportedSort.into());
        }
        let rows = self.rel(source, base, schemas)?.term;
        let elem = self.elem_of(&rows)?;
        let x = self.var(elem.clone(), "x")?;
        let y = self.var(elem, "y")?;
        // y sorts before x: lexicographically by the keys, each in its direction and nulls order
        let mut before = self.tm.mk_false();
        for key in keys.iter().rev() {
            let c = key.column as usize;
            let a = self.select(y.clone(), c)?;
            let b = self.select(x.clone(), c)?;
            let a_null = self.is_null(a.clone())?;
            let b_null = self.is_null(b.clone())?;
            let (va, vb) = (self.val(a.clone())?, self.val(b.clone())?);
            let values = if key.descending { self.less(vb, va)? } else { self.less(va, vb)? };
            let b_some = self.not(b_null.clone())?;
            let null_first = if key.nulls_first { b_some } else { self.tm.mk_false() };
            let some_first = if key.nulls_first { self.tm.mk_false() } else { self.tm.mk_true() };
            let rest = self.ite(b_null, some_first, values)?;
            let less = self.ite(a_null, null_first, rest)?;
            let same = self.tm.mk_term(Kind::Equal, &[a, b])?;
            let tie = self.and(vec![same, before])?;
            before = self.or(vec![less, tie])?;
        }
        let p = self.lambda(&[y.clone()], before)?;
        let earlier = self.tm.mk_term(self.filter_kind(), &[p, rows.clone()])?;
        // the place: a fold counting the rows before x
        let int = self.tm.integer_sort();
        let z = self.var(self.elem_of(&rows)?, "z")?;
        let a = self.var(int, "a")?;
        let one = self.tm.mk_integer(1);
        let step = self.tm.mk_term(Kind::Add, &[a.clone(), one])?;
        let count = self.lambda(&[z, a], step)?;
        let zero = self.tm.mk_integer(0);
        let place = self.tm.mk_term(Kind::BagFold, &[count, zero, earlier])?;
        let first = self.tm.mk_integer(off);
        let mut kept = vec![self.tm.mk_term(Kind::Geq, &[place.clone(), first])?];
        if let Some(l) = lim {
            let end = self.tm.mk_integer(off + l);
            kept.push(self.tm.mk_term(Kind::Lt, &[place, end])?);
        }
        let body = self.and(kept)?;
        let q = self.lambda(&[x], body)?;
        Ok(Rel { term: self.tm.mk_term(self.filter_kind(), &[q, rows])? })
    }

    /// The two branches of a set operation, which cvc5 needs over one tuple sort.
    fn sides(&mut self, sides: &[Box<Relation>; 2], base: usize, schemas: &[Schema]) -> R<(Term, Term)> {
        let l = self.rel(&sides[0], base, schemas)?;
        let r = self.rel(&sides[1], base, schemas)?;
        if l.term.sort() != r.term.sort() {
            return Err(unsupported(format!(
                "set operation over branches of different row sorts: {} against {}",
                self.elem_of(&l.term)?,
                self.elem_of(&r.term)?
            )));
        }
        Ok((l.term, r.term))
    }

    /// A table: a constant of the collection sort, with its `NOT NULL` columns and keys asserted on
    /// the first scan (`Cvc5AbstractTranslator.assertKeys`).
    fn scan(&mut self, i: usize, schemas: &[Schema]) -> R<Rel> {
        if let Some((_, t)) = &self.tables[i] {
            return Ok(Rel { term: t.clone() });
        }
        let s = &schemas[i];
        let row = self.row_sort(&s.types)?;
        let table_sort = self.table_sort(row.clone())?;
        let name = s.name.clone();
        let t = self.tm.mk_const(table_sort.clone(), &name)?;
        self.declarations.push(format!("(declare-const {t} {table_sort})"));
        self.tables[i] = Some((name.clone(), t.clone()));
        let sorts = row.tuple_element_sorts()?;
        // A NOT NULL column holds no null: the bag of its values has no null in it.
        for (j, nullable) in s.nullable.iter().enumerate().take(s.types.len()) {
            if !nullable {
                let no_null = self.no_null_in(t.clone(), j, &sorts[j])?;
                self.constraints.push((format!("{name}: column {j} is NOT NULL"), no_null));
            }
        }
        // A key: no two rows agree on it, so its values form a set. Under set semantics a table
        // cannot repeat a row in the first place, and the rest of the constraint would need
        // cardinalities; Java asserts nothing there either.
        if self.sem == Semantics::Bags {
            for key in &s.key {
                if key.iter().any(|c| *c >= s.types.len()) {
                    continue;
                }
                let idx: Vec<u32> = key.iter().map(|c| *c as u32).collect();
                let projection = self.project(&idx, t.clone())?;
                let set = self.distinct(projection.clone())?;
                let distinct = self.tm.mk_term(Kind::Equal, &[set, projection])?;
                let cols: Vec<String> = key.iter().map(|c| c.to_string()).collect();
                self.constraints.push((
                    format!("{name}: key ({}): no two rows agree on it, so its values form a set", cols.join(", ")),
                    distinct,
                ));
            }
        }
        Ok(Rel { term: t })
    }

    /// `(= (bag.count (tuple null) ((_ table.project j) T)) 0)`, or under sets that the null tuple
    /// is not a member of the projection.
    fn no_null_in(&mut self, table: Term, j: usize, col_sort: &Sort) -> R<Term> {
        let null = self.null(col_sort.clone())?;
        Ok(match self.sem {
            Semantics::Bags => {
                // `(= (bag.filter (lambda t. (= t.j null)) T) empty)`: an emptiness the solver
                // decides with one lemma per row it knows, where a count of a projection would
                // bring the whole map machinery (preimage sums) to every table column
                let elem = self.elem_of(&table)?;
                let t = self.var(elem, "n")?;
                let cell = self.select(t.clone(), j)?;
                let is_null = self.tm.mk_term(Kind::Equal, &[cell, null])?;
                let p = self.lambda(&[t], is_null)?;
                let nulls = self.tm.mk_term(self.filter_kind(), &[p, table])?;
                self.is_empty(nulls)?
            }
            Semantics::Sets => {
                let column = self.project(&[j as u32], table)?;
                let null_row = self.tm.mk_tuple(&[null])?;
                let member = self.member(null_row, column)?;
                self.not(member)?
            }
        })
    }

    fn filter(&mut self, condition: &Expr, table: Term, base: usize, nonnull: Vec<bool>, schemas: &[Schema]) -> R<Rel> {
        let elem = self.elem_of(&table)?;
        let t = self.var(elem, "t")?;
        let row = Row { var: t.clone(), base, nonnull };
        let body = self.condition(condition, &row, schemas)?;
        let p = self.lambda(&[t], body)?;
        Ok(Rel { term: self.tm.mk_term(self.filter_kind(), &[p, table])? })
    }

    /// Column references alone are the cheap `table.project`; anything else maps a lambda.
    fn projection(&mut self, source: &Relation, target: &[Expr], base: usize, schemas: &[Schema]) -> R<Rel> {
        let src = self.rel(source, base, schemas)?;
        let width = self.width(&src.term.sort())?;
        let mut indices = Vec::with_capacity(target.len());
        for e in target {
            match e {
                Expr::Column { index, .. } if (*index as usize) >= base => {
                    let i = *index as usize;
                    if i - base >= width {
                        return Err(TranslateError::ColumnOutOfRange.into());
                    }
                    indices.push((i - base) as u32);
                }
                // an expression, or a column of an enclosing row: a map over the rows
                _ => {
                    indices.clear();
                    break;
                }
            }
        }
        if indices.len() == target.len() && !target.is_empty() {
            return Ok(Rel { term: self.project(&indices, src.term)? });
        }
        let elem = self.elem_of(&src.term)?;
        let t = self.var(elem, "t")?;
        let row = Row { var: t.clone(), base, nonnull: never_null_columns(source, base, schemas) };
        let cells: Vec<Term> = target.iter().map(|e| self.expr(e, &row, schemas)).collect::<R<_>>()?;
        let body = self.tm.mk_tuple(&cells)?;
        let f = self.lambda(&[t], body)?;
        Ok(Rel { term: self.tm.mk_term(self.map_kind(), &[f, src.term])? })
    }

    fn values(&mut self, schema: &[Type], content: &[Vec<Expr>], base: usize, schemas: &[Schema]) -> R<Rel> {
        let row_sort = self.row_sort(schema)?;
        let table_sort = self.table_sort(row_sort.clone())?;
        if content.is_empty() {
            return Ok(Rel { term: self.empty_table(table_sort)? });
        }
        // A literal row needs no row in scope; the dummy is never read.
        let dummy = self.var(row_sort, "v")?;
        let row = Row { var: dummy, base, nonnull: Vec::new() };
        let mut rows = Vec::with_capacity(content.len());
        for cells in content {
            if cells.len() != schema.len() {
                return Err(TranslateError::MalformedShape("values row width".into()).into());
            }
            let mut terms = Vec::with_capacity(cells.len());
            for (cell, ty) in cells.iter().zip(schema) {
                let t = self.expr(cell, &row, schemas)?;
                terms.push(self.as_type(t, *ty)?);
            }
            let tuple = self.tm.mk_tuple(&terms)?;
            rows.push(self.singleton(tuple)?);
        }
        let mut union = rows[0].clone();
        for r in &rows[1..] {
            union = self.tm.mk_term(self.union_all_kind(), &[union, r.clone()])?;
        }
        Ok(Rel { term: union })
    }

    fn join(
        &mut self,
        left: &Relation,
        right: &Relation,
        kind: JoinKind,
        condition: &Expr,
        base: usize,
        schemas: &[Schema],
    ) -> R<Rel> {
        let a = self.rel(left, base, schemas)?.term;
        let b = self.rel(right, base, schemas)?.term;
        let mut product = self.tm.mk_term(self.product_kind(), &[a.clone(), b.clone()])?;
        let mut nonnull = never_null_columns(left, base, schemas);
        nonnull.extend(never_null_columns(right, base, schemas));
        if !is_true(condition) {
            product = self.filter(condition, product, base, nonnull.clone(), schemas)?.term;
        }
        Ok(Rel {
            term: match kind {
                JoinKind::Inner => product,
                JoinKind::Left => {
                    let padded = self.unmatched(&a, &b, condition, Side::Left, base, &nonnull, schemas)?;
                    self.tm.mk_term(self.union_all_kind(), &[padded, product])?
                }
                JoinKind::Right => {
                    let padded = self.unmatched(&a, &b, condition, Side::Right, base, &nonnull, schemas)?;
                    self.tm.mk_term(self.union_all_kind(), &[padded, product])?
                }
                JoinKind::Full => {
                    let l = self.unmatched(&a, &b, condition, Side::Left, base, &nonnull, schemas)?;
                    let r = self.unmatched(&a, &b, condition, Side::Right, base, &nonnull, schemas)?;
                    let both = self.tm.mk_term(self.union_all_kind(), &[l, r])?;
                    self.tm.mk_term(self.union_all_kind(), &[both, product])?
                }
            },
        })
    }

    /// The rows of one side that no row of the other joins with, each padded with nulls on the
    /// other side: `(bag.filter (lambda u. (= (bag.filter (lambda v. cond(u, v)) other) empty)) side)`,
    /// mapped onto the padded row. A row matching `k` rows contributes no padded row while an
    /// unmatched one keeps every copy, as with the difference `side - project(matched)`, but the
    /// anti-join is a filter whose predicate the solver can compare with another side's predicate
    /// pointwise, where a difference of two projections asks for the multiplicities of both.
    #[allow(clippy::too_many_arguments)]
    fn unmatched(
        &mut self,
        a: &Term,
        b: &Term,
        condition: &Expr,
        side: Side,
        base: usize,
        nonnull: &[bool],
        schemas: &[Schema],
    ) -> R<Term> {
        let (side_table, other) = match side {
            Side::Left => (a, b),
            Side::Right => (b, a),
        };
        let s_elem = self.elem_of(side_table)?;
        let o_elem = self.elem_of(other)?;
        let n = s_elem.tuple_length()?;
        let m = o_elem.tuple_length()?;
        let u = self.var(s_elem.clone(), "u")?;
        let v = self.var(o_elem.clone(), "v")?;
        // the product row, left columns then right columns, from the two candidates
        let (left_var, left_n, right_var, right_n) = match side {
            Side::Left => (u.clone(), n, v.clone(), m),
            Side::Right => (v.clone(), m, u.clone(), n),
        };
        let mut cells = Vec::with_capacity(left_n + right_n);
        for i in 0..left_n {
            cells.push(self.select(left_var.clone(), i)?);
        }
        for i in 0..right_n {
            cells.push(self.select(right_var.clone(), i)?);
        }
        let row = self.tm.mk_tuple(&cells)?;
        let matches = if is_true(condition) {
            other.clone()
        } else {
            let body = self.condition(condition, &Row { var: row, base, nonnull: nonnull.to_vec() }, schemas)?;
            let p = self.lambda(&[v], body)?;
            self.tm.mk_term(self.filter_kind(), &[p, other.clone()])?
        };
        let none = self.is_empty(matches)?;
        let anti = self.lambda(&[u], none)?;
        let unmatched = self.tm.mk_term(self.filter_kind(), &[anti, side_table.clone()])?;
        // pad: the side's columns in place, nulls for the other side's
        let a_sorts = self.column_sorts(a)?;
        let b_sorts = self.column_sorts(b)?;
        let offset = match side {
            Side::Left => 0,
            Side::Right => a_sorts.len(),
        };
        let t = self.var(s_elem, "t")?;
        let mut padded = Vec::with_capacity(a_sorts.len() + b_sorts.len());
        for (i, sort) in a_sorts.iter().chain(b_sorts.iter()).enumerate() {
            if i >= offset && i < offset + n {
                padded.push(self.select(t.clone(), i - offset)?);
            } else {
                padded.push(self.null(sort.clone())?);
            }
        }
        let body = self.tm.mk_tuple(&padded)?;
        let f = self.lambda(&[t], body)?;
        Ok(self.tm.mk_term(self.map_kind(), &[f, unmatched])?)
    }

    // ── Aggregation (Cvc5AbstractTranslator.translate(LogicalAggregate)) ────────────────────────

    fn group(&mut self, source: &Relation, keys: &[Expr], calls: &[AggCall], base: usize, schemas: &[Schema]) -> R<Rel> {
        let child = self.rel(source, base, schemas)?.term;
        let width = self.width(&child.sort())?;
        let column = |e: &Expr, err: TranslateError| -> R<u32> {
            match e {
                Expr::Column { index, .. } => {
                    let i = *index as usize;
                    if i < base {
                        return Err(TranslateError::Correlated.into());
                    }
                    if i - base >= width {
                        return Err(TranslateError::ColumnOutOfRange.into());
                    }
                    Ok((i - base) as u32)
                }
                _ => Err(err.into()),
            }
        };
        let indices: Vec<u32> = keys.iter().map(|k| column(k, TranslateError::GroupKeyNonRef)).collect::<R<_>>()?;
        if calls.is_empty() {
            // SELECT DISTINCT, or GROUP BY with no aggregate: duplicate removal of a projection.
            let projection = self.project(&indices, child)?;
            return Ok(Rel { term: self.distinct(projection)? });
        }
        // The argument column of each call, or none for COUNT(*). A COUNT of a column that is never
        // NULL counts every row, so it is COUNT(*), one fold for both.
        let mut args: Vec<Option<u32>> = Vec::with_capacity(calls.len());
        // the other columns of a COUNT of several, which counts the rows where none is NULL
        let mut extra: Vec<Vec<u32>> = Vec::with_capacity(calls.len());
        for call in calls {
            if call.operand.len() > 1 && call.operator != "COUNT" {
                return Err(unsupported(format!("multi-argument aggregate {}", call.operator)));
            }
            let cols: Vec<u32> =
                call.operand.iter().map(|e| column(e, TranslateError::AggArgNonRef)).collect::<R<_>>()?;
            let never = cols.iter().all(|c| never_null(source, *c as usize, base, schemas));
            if cols.is_empty() || (call.operator == "COUNT" && !call.distinct && never) {
                args.push(None);
                extra.push(Vec::new());
            } else {
                args.push(Some(cols[0]));
                extra.push(cols[1..].to_vec());
            }
        }
        if calls.iter().any(|c| c.distinct) {
            if args.iter().zip(calls).any(|(a, c)| c.distinct && a.is_none()) {
                return Err(TranslateError::DistinctAggregateUnsupported.into());
            }
            if calls.len() > 1 {
                return self.mixed_distinct(child, &indices, calls, &args, &extra, source, base, schemas);
            }
            // one DISTINCT call: fold over the projection onto (keys, arguments) with duplicates
            // removed, grouped by the keys, which is the plan Calcite writes for it
            let argument = args[0].expect("checked");
            let mut keep = indices.clone();
            keep.push(argument);
            keep.extend(extra[0].iter().copied());
            let projection = self.project(&keep, child)?;
            let dedup = self.distinct(projection)?;
            let nk = indices.len() as u32;
            let mut call = calls[0].clone();
            call.distinct = false;
            let operands: Vec<u32> = std::iter::once(argument).chain(extra[0].iter().copied()).collect();
            let never = operands.iter().all(|c| never_null(source, *c as usize, base, schemas));
            let (arg, ex) = if call.operator == "COUNT" && never {
                (None, Vec::new())
            } else {
                (Some(nk), (nk + 1..nk + operands.len() as u32).collect())
            };
            return Ok(Rel {
                term: self.aggregate_over(dedup, (0..nk).collect(), std::slice::from_ref(&call), vec![arg], &[ex])?,
            });
        }
        Ok(Rel { term: self.aggregate_over(child, indices, calls, args, &extra)? })
    }

    /// A `GROUP BY` with `DISTINCT` calls: `table.aggr` folds over every copy of a row, so each
    /// `DISTINCT` call folds over its own input, the projection onto the keys and its argument with
    /// duplicates removed; the plain calls fold over the rows; the groups of all of them are the
    /// same key values, joined on the keys (equal as values, NULL included, as grouping compares
    /// them). With no key each part is one row and the join is their product. For
    /// `SELECT k, COUNT(DISTINCT c), SUM(a) FROM t GROUP BY k`, the rows of
    /// `(bag.filter (lambda ((p W)) (= ((_ tuple.select 0) p) ((_ tuple.select 2) p)))
    /// (table.product A B))` mapped to `(tuple k count sum)`, with `A` the aggregate of `SUM(a)`
    /// over the rows of `t` by `k`, and `B` that of `COUNT(c)` over
    /// `(bag.setof (bag.map (lambda ((x T)) (tuple ((_ tuple.select 0) x) ((_ tuple.select 1) x))) t))`.
    #[allow(clippy::too_many_arguments)]
    fn mixed_distinct(
        &mut self,
        child: Term,
        indices: &[u32],
        calls: &[AggCall],
        args: &[Option<u32>],
        extra: &[Vec<u32>],
        source: &Relation,
        base: usize,
        schemas: &[Schema],
    ) -> R<Rel> {
        let nk = indices.len();
        let plain: Vec<usize> = (0..calls.len()).filter(|&j| !calls[j].distinct).collect();
        // the first part: the plain calls over the rows, or the distinct keys when there are none
        let first = if plain.is_empty() {
            let projection = self.project(indices, child.clone())?;
            self.distinct(projection)?
        } else {
            let pc: Vec<AggCall> = plain.iter().map(|&j| calls[j].clone()).collect();
            let pa: Vec<Option<u32>> = plain.iter().map(|&j| args[j]).collect();
            let pe: Vec<Vec<u32>> = plain.iter().map(|&j| extra[j].clone()).collect();
            self.aggregate_over(child.clone(), indices.to_vec(), &pc, pa, &pe)?
        };
        // Without GROUP BY the query has its one row even over no rows, and the distinct keys of
        // no columns are no row then: the parts below, folds without keys, are one row each.
        let mut parts = if plain.is_empty() && nk == 0 { Vec::new() } else { vec![first] };
        // where each call's value is: (part, column)
        let mut place: Vec<(usize, usize)> = vec![(0, 0); calls.len()];
        for (n, &j) in plain.iter().enumerate() {
            place[j] = (0, nk + n);
        }
        for j in 0..calls.len() {
            if !calls[j].distinct {
                continue;
            }
            let argument = args[j].expect("checked");
            let mut keep = indices.to_vec();
            keep.push(argument);
            keep.extend(extra[j].iter().copied());
            let projection = self.project(&keep, child.clone())?;
            let dedup = self.distinct(projection)?;
            let mut call = calls[j].clone();
            call.distinct = false;
            // over the deduplicated rows a COUNT of columns that are never NULL counts them all
            let operands: Vec<u32> = std::iter::once(argument).chain(extra[j].iter().copied()).collect();
            let never = operands.iter().all(|c| never_null(source, *c as usize, base, schemas));
            let (arg, ex) = if call.operator == "COUNT" && never {
                (None, Vec::new())
            } else {
                (Some(nk as u32), (nk as u32 + 1..(nk + operands.len()) as u32).collect())
            };
            let part =
                self.aggregate_over(dedup, (0..nk as u32).collect(), std::slice::from_ref(&call), vec![arg], &[ex])?;
            place[j] = (parts.len(), nk);
            parts.push(part);
        }
        // the product of the parts, right-nested, and where each part's columns start in its rows
        let mut widths = Vec::with_capacity(parts.len());
        for p in &parts {
            widths.push(self.width(&p.sort())?);
        }
        let mut product = parts.last().expect("one part").clone();
        for p in parts.iter().rev().skip(1) {
            product = self.tm.mk_term(self.product_kind(), &[p.clone(), product])?;
        }
        let mut offsets = Vec::with_capacity(parts.len());
        let mut at = 0;
        for w in &widths {
            offsets.push(at);
            at += w;
        }
        let elem = self.elem_of(&product)?;
        let t = self.var(elem, "t")?;
        let mut equal = Vec::new();
        for p in 1..parts.len() {
            for k in 0..nk {
                let a = self.select(t.clone(), k)?;
                let b = self.select(t.clone(), offsets[p] + k)?;
                equal.push(self.tm.mk_term(Kind::Equal, &[a, b])?);
            }
        }
        let joined = if equal.is_empty() {
            product
        } else {
            let body = self.and(equal)?;
            let p = self.lambda(&[t.clone()], body)?;
            self.tm.mk_term(self.filter_kind(), &[p, product])?
        };
        let mut cells = Vec::with_capacity(nk + calls.len());
        for k in 0..nk {
            cells.push(self.select(t.clone(), k)?);
        }
        for &(p, c) in &place {
            cells.push(self.select(t.clone(), offsets[p] + c)?);
        }
        let body = self.tm.mk_tuple(&cells)?;
        let f = self.lambda(&[t], body)?;
        Ok(Rel { term: self.tm.mk_term(self.map_kind(), &[f, joined])? })
    }

    /// `GROUP BY` over `child` by the columns `indices`, the calls folding over every row, each
    /// reading column `args[j]` (none for `COUNT(*)`), and `extra[j]` the further columns of a
    /// `COUNT` of several. For `SELECT k, COUNT(*) FROM t GROUP BY k`, k column 0,
    /// `((_ table.project 0 1) (bag.filter (lambda ((r U)) (>= ((_ tuple.select 2) r) 1))
    /// ((_ table.aggr 0) (lambda ((x T) (y U)) (tuple ((_ tuple.select 0) x) (nullable.some (+
    /// (nullable.val ((_ tuple.select 1) y)) 1)) (+ ((_ tuple.select 2) y) 1))) (tuple (as
    /// nullable.null (Nullable Int)) (nullable.some 0) 0) t)))`: the last column counts the rows of
    /// the group, and the filter drops the row of initial values `table.aggr` gives an empty `t`.
    fn aggregate_over(
        &mut self,
        child: Term,
        indices: Vec<u32>,
        calls: &[AggCall],
        args: Vec<Option<u32>>,
        extra: &[Vec<u32>],
    ) -> R<Term> {
        let x_sort = self.elem_of(&child)?;
        let x_sorts = x_sort.tuple_element_sorts()?;
        // The accumulator: one slot per key, then the slots of each call (AVG keeps a sum and a
        // count, and is finished off by a map below).
        let mut slot_sorts: Vec<Sort> = indices.iter().map(|i| x_sorts[*i as usize].clone()).collect();
        let mut slots: Vec<Vec<Sort>> = Vec::with_capacity(calls.len());
        // A SUM of integers the IR types as REAL (see the frontend's `numeric` guard) adds them as
        // integers, read as a real once at the end: the same value, which stays visibly an integer.
        let int = self.col_sort(Type::Integer)?;
        let mut int_sums = vec![false; calls.len()];
        for (j, call) in calls.iter().enumerate() {
            if call.operator == "SUM" && call.ty == Type::Real {
                if let Some(i) = args[j] {
                    int_sums[j] = x_sorts[i as usize] == int;
                }
            }
        }
        for (j, call) in calls.iter().enumerate() {
            let out = if int_sums[j] { int.clone() } else { self.col_sort(call.ty)? };
            let s = match call.operator.as_str() {
                "AVG" => {
                    let int = self.col_sort(Type::Integer)?;
                    vec![out, int]
                }
                _ => vec![out],
            };
            slot_sorts.extend(s.iter().cloned());
            slots.push(s);
        }
        // GROUP BY over an empty table has no groups, so SQL answers with no rows, while
        // table.aggr gives the empty table one empty part folded to a row of initial values. A
        // hidden counter tells that row apart: it is the one with count 0, filtered out below and
        // projected away again. (An ite over the whole table would hide the aggregate from the
        // solver behind a fresh constant.)
        let counted = !indices.is_empty();
        if counted {
            slot_sorts.push(self.tm.integer_sort());
        }
        let y_sort = self.tm.mk_tuple_sort(&slot_sorts)?;
        let x = self.var(x_sort.clone(), "x")?;
        let y = self.var(y_sort.clone(), "y")?;
        let mut elements = Vec::with_capacity(slot_sorts.len());
        let mut initial = Vec::with_capacity(slot_sorts.len());
        for (k, i) in indices.iter().enumerate() {
            // Never read: the first row of each group overwrites it. It only has to typecheck.
            elements.push(self.select(x.clone(), *i as usize)?);
            initial.push(self.null(slot_sorts[k].clone())?);
        }
        let mut at = indices.len();
        for (j, call) in calls.iter().enumerate() {
            let arg = match args[j] {
                None => None,
                // COUNT reads only whether the argument is null; the others fold it into the slot.
                Some(i) if call.operator == "COUNT" => Some(self.select(x.clone(), i as usize)?),
                Some(i) => {
                    let a = self.select(x.clone(), i as usize)?;
                    Some(self.as_sort(a, &slots[j][0])?)
                }
            };
            let accs: Vec<Term> = (0..slots[j].len()).map(|s| self.select(y.clone(), at + s)).collect::<R<_>>()?;
            if let (Some(first), Some(more)) = (arg.clone(), extra.get(j).filter(|e| !e.is_empty())) {
                // COUNT of several columns: the rows where none of them is NULL
                let mut nulls = vec![self.is_null(first)?];
                for c in more {
                    let a = self.select(x.clone(), *c as usize)?;
                    nulls.push(self.is_null(a)?);
                }
                let any = self.or(nulls)?;
                let acc = accs[0].clone();
                let v = self.val(acc.clone())?;
                let one = self.tm.mk_integer(1);
                let next = self.tm.mk_term(Kind::Add, &[v, one])?;
                let next = self.some(next)?;
                elements.push(self.ite(any, acc, next)?);
                let zero = self.tm.mk_integer(0);
                initial.push(self.some(zero)?);
                at += slots[j].len();
                continue;
            }
            let (e, i) = self.aggregate_step(call, arg, &accs, &slots[j])?;
            elements.extend(e);
            initial.extend(i);
            at += slots[j].len();
        }
        if counted {
            let last = slot_sorts.len() - 1;
            let acc = self.select(y.clone(), last)?;
            let one = self.tm.mk_integer(1);
            elements.push(self.tm.mk_term(Kind::Add, &[acc, one])?);
            initial.push(self.tm.mk_integer(0));
        }
        let body = self.tm.mk_tuple(&elements)?;
        let init = self.tm.mk_tuple(&initial)?;
        let f = self.lambda(&[x, y], body)?;
        let op = self.tm.mk_op(self.aggregate_kind(), &indices)?;
        let mut aggregated = self.tm.mk_term_from_op(op, &[f, init, child.clone()])?;
        if counted {
            // keep the groups a row reached, then drop the counter
            let last = slot_sorts.len() - 1;
            let r = self.var(y_sort.clone(), "r")?;
            let cnt = self.select(r.clone(), last)?;
            let one = self.tm.mk_integer(1);
            let reached = self.tm.mk_term(Kind::Geq, &[cnt, one])?;
            let p = self.lambda(&[r], reached)?;
            aggregated = self.tm.mk_term(self.filter_kind(), &[p, aggregated])?;
            let keep: Vec<u32> = (0..last as u32).collect();
            aggregated = self.project(&keep, aggregated)?;
        }
        if slots.iter().any(|s| s.len() > 1) || int_sums.iter().any(|b| *b) {
            aggregated = self.finish_aggregates(aggregated, indices.len(), calls, &slots, &int_sums)?;
        }
        // No GROUP BY: SQL asks for exactly one row even over no rows -- SELECT COUNT(*) FROM
        // empty is 0 -- which is what the fold of a single empty group gives.
        Ok(aggregated)
    }

    /// One call's step of the fold and its seed: how the slots change on a row whose argument is
    /// `arg` (`None` for `COUNT(*)`), with `accs` the running values. SQL aggregates skip null
    /// inputs, and SUM/MIN/MAX over no non-null value are null, so those seed with null and treat a
    /// null accumulator as "nothing seen yet"; COUNT starts at zero and never returns null.
    fn aggregate_step(&mut self, call: &AggCall, arg: Option<Term>, accs: &[Term], sorts: &[Sort]) -> R<(Vec<Term>, Vec<Term>)> {
        let one = self.tm.mk_integer(1);
        let zero = self.tm.mk_integer(0);
        let acc = accs[0].clone();
        match call.operator.as_str() {
            "COUNT" => {
                let v = self.val(acc.clone())?;
                let more = self.tm.mk_term(Kind::Add, &[v.clone(), one])?;
                let more = self.some(more)?;
                let step = match &arg {
                    None => more,
                    Some(a) => {
                        let null = self.is_null(a.clone())?;
                        self.ite(null, acc, more)?
                    }
                };
                let seed = self.some(zero)?;
                Ok((vec![step], vec![seed]))
            }
            "SUM" | "MIN" | "MAX" => {
                let a = arg.ok_or_else(|| unsupported(format!("{} without an argument", call.operator)))?;
                let step = self.fold_step(&call.operator, a, acc)?;
                let seed = self.null(sorts[0].clone())?;
                Ok((vec![step], vec![seed]))
            }
            "SINGLE_VALUE" => {
                // Calcite's: the value of the group's only row, an error on two or more. A run
                // that answers reads one row, which a fold keeping the last value read returns;
                // NULL over no rows, as a scalar subquery over none is.
                let a = arg.ok_or_else(|| unsupported("SINGLE_VALUE without an argument"))?;
                let seed = self.null(sorts[0].clone())?;
                Ok((vec![a], vec![seed]))
            }
            "AVG" => {
                let a = arg.ok_or_else(|| unsupported("AVG without an argument"))?;
                let sum = self.fold_step("SUM", a.clone(), acc)?;
                let count_acc = accs[1].clone();
                let v = self.val(count_acc.clone())?;
                let more = self.tm.mk_term(Kind::Add, &[v, one])?;
                let more = self.some(more)?;
                let null = self.is_null(a)?;
                let count = self.ite(null, count_acc, more)?;
                let sum_seed = self.null(sorts[0].clone())?;
                let count_seed = self.some(zero)?;
                Ok((vec![sum, count], vec![sum_seed, count_seed]))
            }
            other => Err(TranslateError::Aggregate(other.to_string()).into()),
        }
    }

    /// `acc` after a row with argument `arg`: a null row leaves it alone, the first non-null row
    /// seeds it, and a later one combines.
    fn fold_step(&mut self, op: &str, arg: Term, acc: Term) -> R<Term> {
        let va = self.val(arg.clone())?;
        let vb = self.val(acc.clone())?;
        let combined = match op {
            "SUM" => {
                let s = self.tm.mk_term(Kind::Add, &[va, vb])?;
                self.some(s)?
            }
            "MIN" => {
                let less = self.less(va, vb)?;
                self.ite(less, arg.clone(), acc.clone())?
            }
            _ => {
                let less = self.less(vb, va)?;
                self.ite(less, arg.clone(), acc.clone())?
            }
        };
        let acc_null = self.is_null(acc.clone())?;
        let seeded = self.ite(acc_null, arg.clone(), combined)?;
        let arg_null = self.is_null(arg)?;
        self.ite(arg_null, acc, seeded)
    }

    /// `a < b` on two values of one sort, in the sort's own order.
    fn less(&mut self, a: Term, b: Term) -> R<Term> {
        let s = a.sort();
        if s.is_integer() || s.is_real() {
            return Ok(self.tm.mk_term(Kind::Lt, &[a, b])?);
        }
        if s.is_string() {
            return Ok(self.tm.mk_term(Kind::StringLt, &[a, b])?);
        }
        if s.is_boolean() {
            // FALSE < TRUE, as Postgres orders booleans.
            let na = self.not(a)?;
            return self.and(vec![na, b]);
        }
        Err(unsupported(format!("an order on values of sort {s}")))
    }

    /// The row of a `GROUP BY` from its accumulator, where a call kept more than one slot: `AVG`
    /// becomes `avg(sum, count)`, null when the count is zero.
    fn finish_aggregates(
        &mut self,
        aggregated: Term,
        nkeys: usize,
        calls: &[AggCall],
        slots: &[Vec<Sort>],
        int_sums: &[bool],
    ) -> R<Term> {
        let y_sort = self.elem_of(&aggregated)?;
        let y = self.var(y_sort, "y")?;
        let mut cells = Vec::with_capacity(nkeys + calls.len());
        for k in 0..nkeys {
            cells.push(self.select(y.clone(), k)?);
        }
        let mut at = nkeys;
        for (j, call_slots) in slots.iter().take(calls.len()).enumerate() {
            let first = self.select(y.clone(), at)?;
            if int_sums[j] {
                cells.push(self.lift(Kind::ToReal, &[first])?);
            } else if call_slots.len() == 1 {
                cells.push(first);
            } else {
                let count = self.select(y.clone(), at + 1)?;
                let sum_base = call_slots[0].nullable_element_sort()?;
                let int = self.tm.integer_sort();
                let out_base = sum_base.clone();
                let avg = self.fun(&format!("avg:{sum_base}"), &[sum_base.clone(), int], out_base)?;
                let n = self.val(count)?;
                let zero = self.tm.mk_integer(0);
                let none = self.tm.mk_term(Kind::Equal, &[n.clone(), zero])?;
                let s = self.val(first.clone())?;
                let v = self.apply(avg, &[s, n])?;
                let some = self.some(v)?;
                let null = self.null(first.sort())?;
                cells.push(self.ite(none, null, some)?);
            }
            at += call_slots.len();
        }
        let body = self.tm.mk_tuple(&cells)?;
        let f = self.lambda(&[y], body)?;
        Ok(self.tm.mk_term(self.map_kind(), &[f, aggregated])?)
    }

    // ── Expressions (Cvc5AbstractTranslator.translateRowExpr) ───────────────────────────────────

    /// `e` over `row`, as a term of the nullable sort of `e`'s type.
    fn expr(&mut self, e: &Expr, row: &Row, schemas: &[Schema]) -> R<Term> {
        let t = match e {
            Expr::Column { index, .. } => {
                let i = *index as usize;
                if i < row.base {
                    // a column of an enclosing query: a correlated subquery
                    for r in self.outer.iter().rev() {
                        let w = r.var.sort().tuple_length()?;
                        if i >= r.base && i - r.base < w {
                            let (var, at) = (r.var.clone(), i - r.base);
                            return self.select(var, at);
                        }
                    }
                    return Err(TranslateError::Correlated.into());
                }
                let width = row.var.sort().tuple_length()?;
                if i - row.base >= width {
                    return Err(TranslateError::ColumnOutOfRange.into());
                }
                return self.select(row.var.clone(), i - row.base);
            }
            Expr::Literal { value, ty } => self.literal(value, *ty)?,
            Expr::Cast { ty, operand } => {
                let v = self.expr(operand, row, schemas)?;
                if operand.ty() == *ty && matches!(ty, Type::Integer | Type::Boolean | Type::Varchar) {
                    // A cast to the type the value already has changes no value: an integer read as
                    // an integer (an overflow raises, and a run that answers has none), a boolean as
                    // a boolean, text as unbounded text. Calcite writes these around nullable
                    // columns. A length-limited VARCHAR is `q_cast_varchar_N`, never this node.
                    return self.as_type(v, e.ty());
                }
                let name = format!("cast:{}->{}", kind_name(operand.ty()), kind_name(*ty));
                let out = self.base_sort(*ty);
                let base = v.sort().nullable_element_sort()?;
                if base.is_integer() && out.is_real() {
                    // an integer read as a `numeric`: the same value
                    let r = self.lift(Kind::ToReal, &[v])?;
                    return self.as_type(r, e.ty());
                }
                let f = self.fun(&name, &[base.clone()], out.clone())?;
                if base.is_real() && out.is_integer() {
                    // `numeric` to an integer rounds, which only a value with a fraction shows: an
                    // integral value is itself, the rest is the uninterpreted cast
                    self.strict(&[v], |enc, vals| {
                        let x = vals[0].clone();
                        let integral = enc.tm.mk_term(Kind::IsInteger, &[x.clone()])?;
                        let exact = enc.tm.mk_term(Kind::ToInteger, &[x.clone()])?;
                        let other = enc.apply(f, &vals)?;
                        enc.ite(integral, exact, other)
                    })?
                } else {
                    self.strict(&[v], |enc, vals| enc.apply(f, &vals))?
                }
            }
            Expr::Call { operator, ty, operand } => self.call(operator, *ty, operand, row, schemas)?,
            Expr::Subquery { operator, ty: _, operand, query } => self.subquery(operator, operand, query, row, schemas)?,
        };
        self.as_type(t, e.ty())
    }

    fn literal(&mut self, value: &str, ty: Type) -> R<Term> {
        let sort = self.col_sort(ty)?;
        if value == "NULL" {
            return self.null(sort);
        }
        let refuse = || Failure::from(TranslateError::Literal(ty.name().to_string()));
        let v = match ty {
            Type::Integer | Type::Date | Type::Time | Type::Timestamp => {
                self.tm.mk_integer_from_str(value).map_err(|_| refuse())?
            }
            Type::Real => self.tm.mk_real_from_str(value).map_err(|_| refuse())?,
            Type::Boolean => match value.to_ascii_lowercase().as_str() {
                "true" => self.tm.mk_true(),
                "false" => self.tm.mk_false(),
                _ => return Err(refuse()),
            },
            Type::Varchar => self.string(value),
            // An opaque literal is a constant of the opaque sort: two spellings may or may not be
            // one value (`'{"a":1}'` and `'{"a": 1}'` as jsonb are), so nothing relates them.
            Type::Varbinary | Type::Interval => {
                let base = self.base_sort(ty);
                let name = format!("lit:{}:{value}", ty.name());
                self.fun(&name, &[], base)?
            }
        };
        self.some(v)
    }

    /// `t` as a term of the nullable sort of `ty`: itself when it already is one, a lifted
    /// `to_real` for an integer read as a real, and a refusal for any other mismatch, which would
    /// be a plan this encoding mistyped. A VARBINARY annotation on a value the encoding computed
    /// from typed operands -- a `COALESCE` over a REAL and an INTEGER, say, which the frontend
    /// types as opaque -- keeps the computed sort: the value is what its operands made it.
    fn as_type(&mut self, t: Term, ty: Type) -> R<Term> {
        let want = self.col_sort(ty)?;
        if ty == Type::Varbinary && !t.sort().nullable_element_sort()?.is_uninterpreted_sort() {
            return Ok(t);
        }
        self.as_sort(t, &want)
    }

    fn as_sort(&mut self, t: Term, want: &Sort) -> R<Term> {
        let have = t.sort();
        if have == *want {
            return Ok(t);
        }
        // `nullable.null` of another sort: a NULL literal the frontend typed by default (INTEGER)
        // where another type is wanted. A null is a null of whatever sort.
        if t.kind() == Kind::ApplyConstructor && t.num_children() == 1 && want.is_nullable() {
            return self.null(want.clone());
        }
        if have.nullable_element_sort()?.is_integer() && want.nullable_element_sort()?.is_real() {
            return self.lift(Kind::ToReal, &[t]);
        }
        Err(unsupported(format!("a value of sort {have} where {want} is expected")))
    }

    /// The sort a `CASE` or `COALESCE` typed `ty` takes: the nullable sort of `ty`, unless `ty` is
    /// the opaque VARBINARY -- the frontend's type for a result it could not type, a `COALESCE`
    /// over a REAL and an INTEGER say -- when it is the branches' own sort, reals if any branch is
    /// real and the rest are integers. A null literal among them has no say (see [`as_sort`]).
    fn result_sort(&mut self, ty: Type, branches: &[Term]) -> R<Sort> {
        if ty != Type::Varbinary {
            return self.col_sort(ty);
        }
        let mut found: Option<Sort> = None;
        let real = self.col_sort(Type::Real)?;
        let int = self.col_sort(Type::Integer)?;
        for b in branches {
            if b.kind() == Kind::ApplyConstructor && b.num_children() == 1 {
                continue;
            }
            let s = b.sort();
            found = Some(match found {
                None => s,
                Some(f) if f == s => f,
                Some(f) if (f == int && s == real) || (f == real && s == int) => real.clone(),
                Some(_) => return self.col_sort(ty),
            });
        }
        match found {
            Some(s) if !s.nullable_element_sort()?.is_uninterpreted_sort() => Ok(s),
            _ => self.col_sort(ty),
        }
    }

    /// Two operands of a comparison over one sort: numerics widened to reals together, and a
    /// typed value beside an opaque one injected into the opaque sort by an uninterpreted function
    /// (the frontend compares the two through a cast it names, and this is that cast's other
    /// half). Anything else is a mismatch this encoding has no reading for.
    fn unify(&mut self, op: &str, a: Term, b: Term) -> R<(Term, Term)> {
        let (a, b) = self.numeric_pair(a, b)?;
        if a.sort() == b.sort() {
            return Ok((a, b));
        }
        let (sa, sb) = (a.sort().nullable_element_sort()?, b.sort().nullable_element_sort()?);
        if sa.is_uninterpreted_sort() && !sb.is_uninterpreted_sort() {
            let b = self.read_in_sort(b, &sa)?;
            return Ok((a, b));
        }
        if sb.is_uninterpreted_sort() && !sa.is_uninterpreted_sort() {
            let a = self.read_in_sort(a, &sb)?;
            return Ok((a, b));
        }
        Err(unsupported(format!("`{op}` between values of sorts {sa} and {sb}")))
    }

    /// `t` read in the uninterpreted sort `into`, through a strict uninterpreted injection.
    fn read_in_sort(&mut self, t: Term, into: &Sort) -> R<Term> {
        let from = t.sort().nullable_element_sort()?;
        let f = self.fun(&format!("as:{from}->{into}"), &[from], into.clone())?;
        self.strict(&[t], |enc, v| enc.apply(f, &v))
    }

    /// Two operands of an arithmetic operator over one numeric sort, or a refusal.
    fn numeric_operands(&mut self, op: &str, a: Term, b: Term) -> R<(Term, Term)> {
        let (a, b) = self.numeric_pair(a, b)?;
        if a.sort() != b.sort() || !self.numeric(&a)? {
            return Err(unsupported(format!(
                "`{op}` between values of sorts {} and {}",
                a.sort().nullable_element_sort()?,
                b.sort().nullable_element_sort()?
            )));
        }
        Ok((a, b))
    }

    /// Two numeric operands over one sort: an integer beside a real is read as a real.
    fn numeric_pair(&mut self, a: Term, b: Term) -> R<(Term, Term)> {
        let (sa, sb) = (a.sort().nullable_element_sort()?, b.sort().nullable_element_sort()?);
        if sa.is_integer() && sb.is_real() {
            let a = self.lift(Kind::ToReal, &[a])?;
            return Ok((a, b));
        }
        if sa.is_real() && sb.is_integer() {
            let b = self.lift(Kind::ToReal, &[b])?;
            return Ok((a, b));
        }
        Ok((a, b))
    }

    fn call(&mut self, op: &str, ty: Type, operand: &[Expr], row: &Row, schemas: &[Schema]) -> R<Term> {
        let args: Vec<Term> = operand.iter().map(|e| self.expr(e, row, schemas)).collect::<R<_>>()?;
        let arity = args.len();
        match (op, arity) {
            ("AND", _) => self.short_circuit(&args, Kind::And, false),
            ("OR", _) => self.short_circuit(&args, Kind::Or, true),
            ("NOT", 1) => self.lift(Kind::Not, &args),
            ("=", 2) | ("<>", 2) => {
                let (a, b) = self.unify(op, args[0].clone(), args[1].clone())?;
                let kind = if op == "=" { Kind::Equal } else { Kind::Distinct };
                self.lift(kind, &[a, b])
            }
            ("<", 2) | ("<=", 2) | (">", 2) | (">=", 2) => {
                let (a, b) = self.unify(op, args[0].clone(), args[1].clone())?;
                self.compare(op, a, b)
            }
            // Integer and numeric arithmetic, as the frontend spells it: the operators themselves,
            // or its `q_arith_<op>_<left>_<right>` functions over INTEGER and REAL. `+`, `-` and `*`
            // are exact in Postgres and here; integer `/` and `%` truncate toward zero; numeric
            // division is left to the frontend's own uninterpreted `q_arith_div_real_*`, since
            // Postgres rounds it to a scale cvc5's rationals do not have.
            ("+", 2) | ("-", 2) | ("*", 2) => {
                let (a, b) = self.numeric_operands(op, args[0].clone(), args[1].clone())?;
                self.lift(arith_kind(op), &[a, b])
            }
            (_, 2) if numeric_arith(op).is_some_and(|(o, _)| matches!(o, "add" | "sub" | "mul")) => {
                let (a, b) = self.numeric_operands(op, args[0].clone(), args[1].clone())?;
                self.lift(arith_kind(numeric_arith(op).expect("checked").0), &[a, b])
            }
            ("/", 2) | ("q_arith_div_integer_integer", 2) => {
                let (a, b) = self.numeric_operands(op, args[0].clone(), args[1].clone())?;
                if a.sort().nullable_element_sort()?.is_integer() {
                    // Postgres truncates integer division toward zero; SMT-LIB's `div` floors.
                    self.strict(&[a, b], |enc, v| enc.truncating_division(v[0].clone(), v[1].clone()))
                } else if op == "/" {
                    self.lift(Kind::Division, &[a, b])
                } else {
                    self.strict_fun(op, ty, operand, &args)
                }
            }
            ("%", 2) | ("q_arith_mod_integer_integer", 2) => {
                let (a, b) = self.numeric_operands(op, args[0].clone(), args[1].clone())?;
                if !a.sort().nullable_element_sort()?.is_integer() {
                    return self.strict_fun(op, ty, operand, &args);
                }
                self.strict(&[a, b], |enc, v| {
                    let q = enc.truncating_division(v[0].clone(), v[1].clone())?;
                    let qb = enc.tm.mk_term(Kind::Mult, &[q, v[1].clone()])?;
                    Ok(enc.tm.mk_term(Kind::Sub, &[v[0].clone(), qb])?)
                })
            }
            ("-", 1) => self.lift(Kind::Neg, &args),
            ("+", 1) => Ok(args[0].clone()),
            ("IS NULL", 1) => {
                let n = self.is_null(args[0].clone())?;
                self.some(n)
            }
            ("IS NOT NULL", 1) => {
                let s = self.is_some(args[0].clone())?;
                self.some(s)
            }
            ("IS TRUE", 1) => {
                let t = self.truth(args[0].clone())?;
                self.some(t)
            }
            ("IS NOT TRUE", 1) => {
                let t = self.truth(args[0].clone())?;
                let n = self.not(t)?;
                self.some(n)
            }
            ("IS DISTINCT FROM", 2) | ("IS NOT DISTINCT FROM", 2) => {
                let (a, b) = self.unify(op, args[0].clone(), args[1].clone())?;
                let (na, nb) = (self.is_null(a.clone())?, self.is_null(b.clone())?);
                let both_null = self.and(vec![na.clone(), nb.clone()])?;
                let one_null = self.or(vec![na, nb])?;
                let (va, vb) = (self.val(a)?, self.val(b)?);
                let differ = self.tm.mk_term(Kind::Distinct, &[va, vb])?;
                let f = self.tm.mk_false();
                let t = self.tm.mk_true();
                let inner = self.ite(one_null, t, differ)?;
                let distinct = self.ite(both_null, f, inner)?;
                let v = if op == "IS DISTINCT FROM" { distinct } else { self.not(distinct)? };
                self.some(v)
            }
            ("CASE", _) => {
                // Calcite's flattened `[cond, then, ..., else]`, always odd. A null condition takes
                // the else branch.
                if arity == 0 || arity.is_multiple_of(2) {
                    return Err(TranslateError::MalformedShape("CASE operand must be [cond, then, ..., else]".into()).into());
                }
                let branches: Vec<Term> = args.iter().skip(1).step_by(2).cloned().chain([args[arity - 1].clone()]).collect();
                let want = self.result_sort(ty, &branches)?;
                let mut result = self.as_sort(args[arity - 1].clone(), &want)?;
                let mut i = arity - 1;
                while i >= 2 {
                    i -= 2;
                    let c = self.truth(args[i].clone())?;
                    let then = self.as_sort(args[i + 1].clone(), &want)?;
                    result = self.ite(c, then, result)?;
                }
                Ok(result)
            }
            ("COALESCE", _) if arity >= 1 => {
                let want = self.result_sort(ty, &args)?;
                let mut result = self.as_sort(args[arity - 1].clone(), &want)?;
                for a in args[..arity - 1].iter().rev() {
                    let a = self.as_sort(a.clone(), &want)?;
                    let s = self.is_some(a.clone())?;
                    result = self.ite(s, a, result)?;
                }
                Ok(result)
            }
            ("NULLIF", 2) => {
                let (a, b) = self.unify(op, args[0].clone(), args[1].clone())?;
                let eq = self.lift(Kind::Equal, &[a.clone(), b])?;
                let equal = self.truth(eq)?;
                let null = self.null(a.sort())?;
                self.ite(equal, null, a)
            }
            // Interpreted only where the frontend typed the call as cvc5 would: a `UPPER` it types
            // VARBINARY is a function it does not know, and so stays uninterpreted here, named
            // alike on both sides.
            ("||", 2) if ty == Type::Varchar && self.all_strings(&args)? => self.lift(Kind::StringConcat, &args),
            ("SUBSTRING", 2 | 3) if ty == Type::Varchar && self.all_strings(&args[..1])? && self.all_integers(&args[1..])? => {
                // Postgres counts from 1 and clips: characters a .. a + n - 1 that exist, none for a
                // negative end. cvc5's str.substr counts from 0 and clips the length to the end, so
                // the start is max(a, 1) - 1 and the length (a + n) - max(a, 1); without FOR, the
                // rest of the string.
                self.strict(&args, |enc, v| {
                    let one = enc.tm.mk_integer(1);
                    let below = enc.tm.mk_term(Kind::Lt, &[v[1].clone(), one.clone()])?;
                    let first = enc.tm.mk_term(Kind::Ite, &[below, one.clone(), v[1].clone()])?;
                    let start = enc.tm.mk_term(Kind::Sub, &[first.clone(), one])?;
                    let length = if v.len() == 3 {
                        let end = enc.tm.mk_term(Kind::Add, &[v[1].clone(), v[2].clone()])?;
                        enc.tm.mk_term(Kind::Sub, &[end, first])?
                    } else {
                        enc.tm.mk_term(Kind::StringLength, &[v[0].clone()])?
                    };
                    Ok(enc.tm.mk_term(Kind::StringSubstr, &[v[0].clone(), start, length])?)
                })
            }
            ("UPPER", 1) if ty == Type::Varchar && self.all_strings(&args)? => self.lift(Kind::StringToUpper, &args),
            ("LOWER", 1) if ty == Type::Varchar && self.all_strings(&args)? => self.lift(Kind::StringToLower, &args),
            ("ABS", 1) if matches!(ty, Type::Integer | Type::Real) && self.numeric(&args[0])? => self.lift(Kind::Abs, &args),
            ("LIKE", 2) => match (&operand[1], self.all_strings(&args)?) {
                (Expr::Literal { value, ty: Type::Varchar }, true) => {
                    let re = self.like_regex(value)?;
                    self.strict(&args[..1], |enc, v| Ok(enc.tm.mk_term(Kind::StringInRegexp, &[v[0].clone(), re])?))
                }
                _ => self.strict_fun(op, ty, operand, &args),
            },
            _ if strict_function(op) => self.strict_fun(op, ty, operand, &args),
            _ => self.opaque_fun(op, ty, operand, &args),
        }
    }

    fn all_strings(&self, args: &[Term]) -> R<bool> {
        for a in args {
            if !a.sort().nullable_element_sort()?.is_string() {
                return Ok(false);
            }
        }
        Ok(true)
    }

    /// Whether every argument is a nullable integer, `(Nullable Int)`.
    fn all_integers(&self, args: &[Term]) -> R<bool> {
        for a in args {
            if !a.sort().nullable_element_sort()?.is_integer() {
                return Ok(false);
            }
        }
        Ok(true)
    }

    fn numeric(&self, a: &Term) -> R<bool> {
        let s = a.sort().nullable_element_sort()?;
        Ok(s.is_integer() || s.is_real())
    }

    /// `a op b` for an order comparison, by the operands' sort: integers and reals in their order,
    /// strings by code point (`str.<`), booleans with FALSE before TRUE, and an uninterpreted
    /// strict predicate on a sort with no order cvc5 knows.
    fn compare(&mut self, op: &str, a: Term, b: Term) -> R<Term> {
        let s = a.sort().nullable_element_sort()?;
        if s.is_integer() || s.is_real() {
            let kind = match op {
                "<" => Kind::Lt,
                "<=" => Kind::Leq,
                ">" => Kind::Gt,
                _ => Kind::Geq,
            };
            return self.lift(kind, &[a, b]);
        }
        if s.is_string() {
            let (kind, swap) = match op {
                "<" => (Kind::StringLt, false),
                "<=" => (Kind::StringLeq, false),
                ">" => (Kind::StringLt, true),
                _ => (Kind::StringLeq, true),
            };
            return if swap { self.lift(kind, &[b, a]) } else { self.lift(kind, &[a, b]) };
        }
        if s.is_boolean() {
            return self.strict(&[a, b], |enc, v| {
                let (x, y) = (v[0].clone(), v[1].clone());
                match op {
                    "<" => enc.less(x, y),
                    ">" => enc.less(y, x),
                    "<=" => {
                        let l = enc.less(y, x)?;
                        enc.not(l)
                    }
                    _ => {
                        let l = enc.less(x, y)?;
                        enc.not(l)
                    }
                }
            });
        }
        let name = format!("{op}:{s}");
        let bool = self.tm.boolean_sort();
        let f = self.fun(&name, &[s.clone(), s], bool)?;
        self.strict(&[a, b], |enc, v| enc.apply(f, &v))
    }

    /// Postgres's integer division: toward zero. `(div |a| |b|)` is that quotient's magnitude, and
    /// the sign is the operands'. Division by zero is an error in Postgres; here it is whatever
    /// cvc5 makes of `div` by zero, which no interpretation of the pair depends on.
    fn truncating_division(&mut self, a: Term, b: Term) -> R<Term> {
        let zero = self.tm.mk_integer(0);
        let abs_a = self.tm.mk_term(Kind::Abs, std::slice::from_ref(&a))?;
        let abs_b = self.tm.mk_term(Kind::Abs, std::slice::from_ref(&b))?;
        let q = self.tm.mk_term(Kind::IntsDivision, &[abs_a, abs_b])?;
        let neg_q = self.tm.mk_term(Kind::Neg, std::slice::from_ref(&q))?;
        let a_neg = self.tm.mk_term(Kind::Lt, &[a, zero.clone()])?;
        let b_neg = self.tm.mk_term(Kind::Lt, &[b, zero])?;
        let same_sign = self.tm.mk_term(Kind::Equal, &[a_neg, b_neg])?;
        self.ite(same_sign, q, neg_q)
    }

    /// Three-valued `AND`/`OR` over nullable booleans (`Cvc5AbstractTranslator.mkShortCircuit`):
    /// the dominant value -- FALSE for AND, TRUE for OR -- wins however many other operands are
    /// null, so it is tested for first across every operand, and the lift decides the rest.
    fn short_circuit(&mut self, args: &[Term], kind: Kind, dominant: bool) -> R<Term> {
        if args.len() == 1 {
            return Ok(args[0].clone());
        }
        let mut known = Vec::with_capacity(args.len());
        for a in args {
            let s = self.is_some(a.clone())?;
            let v = self.val(a.clone())?;
            let v = if dominant { v } else { self.not(v)? };
            known.push(self.and(vec![s, v])?);
        }
        let is_dominant = self.or(known)?;
        let d = self.tm.mk_boolean(dominant);
        let d = self.some(d)?;
        let lifted = self.lift(kind, args)?;
        self.ite(is_dominant, d, lifted)
    }

    /// `s` as a cvc5 string. cvc5 refuses a character beyond printable ASCII in a plain string, so
    /// every other character, and the backslash, is written as its SMT-LIB escape: `é` as `\u{e9}`.
    /// (`Cvc5AbstractTranslator.mkString` does the same.)
    fn string(&mut self, s: &str) -> Term {
        let mut escaped = String::with_capacity(s.len());
        for c in s.chars() {
            if (' '..='~').contains(&c) && c != '\\' {
                escaped.push(c);
            } else {
                escaped.push_str(&format!("\\u{{{:x}}}", c as u32));
            }
        }
        self.tm.mk_string(&escaped, true)
    }

    /// `pattern` as a regular expression: `%` any string, `_` any character, a backslash escaping
    /// the next character, as Postgres reads `LIKE` without an `ESCAPE` clause.
    fn like_regex(&mut self, pattern: &str) -> R<Term> {
        let mut parts = Vec::new();
        let mut literal = String::new();
        let mut chars = pattern.chars();
        let flush = |enc: &mut Encoder, literal: &mut String, parts: &mut Vec<Term>| -> R<()> {
            if !literal.is_empty() {
                let s = enc.string(literal);
                parts.push(enc.tm.mk_term(Kind::StringToRegexp, &[s])?);
                literal.clear();
            }
            Ok(())
        };
        while let Some(c) = chars.next() {
            match c {
                '%' => {
                    flush(self, &mut literal, &mut parts)?;
                    let any = self.tm.mk_regexp_allchar();
                    parts.push(self.tm.mk_term(Kind::RegexpStar, &[any])?);
                }
                '_' => {
                    flush(self, &mut literal, &mut parts)?;
                    parts.push(self.tm.mk_regexp_allchar());
                }
                '\\' => literal.push(chars.next().unwrap_or('\\')),
                c => literal.push(c),
            }
        }
        flush(self, &mut literal, &mut parts)?;
        Ok(match parts.len() {
            0 => {
                let empty = self.tm.mk_string("", false);
                self.tm.mk_term(Kind::StringToRegexp, &[empty])?
            }
            1 => parts.pop().expect("one"),
            _ => self.tm.mk_term(Kind::RegexpConcat, &parts)?,
        })
    }

    /// A function the encoding does not interpret but knows to be strict: null exactly when an
    /// argument is, and otherwise an uninterpreted function of the argument values, named by its
    /// operand and result types as `sqleq-solver` names it.
    fn strict_fun(&mut self, op: &str, ty: Type, operand: &[Expr], args: &[Term]) -> R<Term> {
        let name = typed_symbol(op, operand, ty);
        let domain: Vec<Sort> = args.iter().map(|a| a.sort().nullable_element_sort()).collect::<Result<_, _>>()?;
        let codomain = self.base_sort(ty);
        let f = self.fun(&name, &domain, codomain)?;
        if args.is_empty() {
            return self.some(f);
        }
        self.strict(args, |enc, v| enc.apply(f, &v))
    }

    /// Any other function: an uninterpreted function over the nullable sorts, so nothing is assumed
    /// about how it treats NULL. Parameter carriers (`QPn`) land here, since a parameter may be
    /// bound to NULL.
    fn opaque_fun(&mut self, op: &str, ty: Type, operand: &[Expr], args: &[Term]) -> R<Term> {
        let name = format!("opaque:{}", typed_symbol(op, operand, ty));
        let domain: Vec<Sort> = args.iter().map(Term::sort).collect();
        let codomain = self.col_sort(ty)?;
        let f = self.fun(&name, &domain, codomain)?;
        self.apply(f, args)
    }

    // ── Subqueries ────────────────────────────────────────────────────────────────────────────────

    fn subquery(&mut self, op: &str, operand: &[Expr], query: &Relation, row: &Row, schemas: &[Schema]) -> R<Term> {
        let inner_base = row.base + row.var.sort().tuple_length()?;
        self.outer.push(row.clone());
        self.depth += 1;
        let inner = self.rel(query, inner_base, schemas);
        self.depth -= 1;
        self.outer.pop();
        let inner = inner?.term;
        match op {
            "EXISTS" => {
                let empty = self.is_empty(inner)?;
                let some_row = self.not(empty)?;
                self.some(some_row)
            }
            "IN" => self.in_subquery(operand, inner, query, inner_base, row, schemas),
            "$SCALAR_QUERY" => {
                let sorts = self.column_sorts(&inner)?;
                if sorts.len() != 1 {
                    return Err(TranslateError::ScalarSubqueryArity.into());
                }
                // At most one row, or Postgres raises an error: the database is one on which the
                // query runs at all.
                let card = self.card(inner.clone())?;
                let one = self.tm.mk_integer(1);
                let at_most_one = self.tm.mk_term(Kind::Leq, &[card, one])?;
                self.constraints.push(("a scalar subquery returns at most one row".into(), at_most_one));
                if self.sem == Semantics::Bags {
                    // The column of the row, or NULL where there is none: a fold that keeps the
                    // value of the row it reads, seeded with NULL. `SINGLE_VALUE`, which Calcite
                    // decorrelates a scalar subquery into, folds its group the same way, so the
                    // two are one term once the solver rewrites the fold over the projection.
                    let elem = self.elem_of(&inner)?;
                    let x = self.var(elem, "x")?;
                    let acc = self.var(sorts[0].clone(), "acc")?;
                    let body = self.select(x.clone(), 0)?;
                    let f = self.lambda(&[x, acc], body)?;
                    let null = self.null(sorts[0].clone())?;
                    return Ok(self.tm.mk_term(Kind::BagFold, &[f, null, inner])?);
                }
                let empty = self.is_empty(inner.clone())?;
                let chosen = self.choose(inner)?;
                let value = self.select(chosen, 0)?;
                let null = self.null(sorts[0].clone())?;
                self.ite(empty, null, value)
            }
            other => Err(TranslateError::Subquery(other.to_string()).into()),
        }
    }

    /// `l IN (subquery)`: TRUE iff some row equals the operand; else UNKNOWN iff some row might --
    /// the operand is null and the subquery has a row, or a row of the subquery is null -- else
    /// FALSE. The reading that makes `NOT IN` reject every row once the subquery holds a null.
    ///
    /// One column only. For `(l1, l2) IN (...)` the rows that might match are those equal to the
    /// operand wherever both are non-null, a filter whose predicate reads the enclosing row: a bag
    /// term under a binder, which the bags solver does not decide. Such a pair is refused.
    fn in_subquery(
        &mut self,
        operand: &[Expr],
        inner: Term,
        query: &Relation,
        inner_base: usize,
        row: &Row,
        schemas: &[Schema],
    ) -> R<Term> {
        let sorts = self.column_sorts(&inner)?;
        if sorts.len() != operand.len() {
            return Err(TranslateError::MalformedShape("IN operand arity does not match subquery width".into()).into());
        }
        if operand.len() != 1 {
            return self.in_subquery_tuple(operand, inner, query, inner_base, row, schemas);
        }
        let l = self.expr(&operand[0], row, schemas)?;
        let l = self.as_sort(l, &sorts[0])?;
        let tuple = self.tm.mk_tuple(std::slice::from_ref(&l))?;
        let operand_never_null = match &operand[0] {
            Expr::Column { index, .. } => {
                (*index as usize).checked_sub(row.base).and_then(|i| row.nonnull.get(i).copied()).unwrap_or(false)
            }
            Expr::Literal { value, .. } => value != "NULL",
            _ => false,
        };
        if operand_never_null && never_null(query, 0, inner_base, schemas) {
            // Neither the operand nor a row of the subquery is NULL: IN is TRUE or FALSE.
            let member = self.member(tuple, inner)?;
            return self.some(member);
        }
        // A NULL operand equals no row, not even a NULL one: `NULL IN (SELECT NULL)` is UNKNOWN.
        let member = self.member(tuple, inner.clone())?;
        let l_null = self.is_null(l)?;
        let l_some = self.not(l_null.clone())?;
        let matches = self.and(vec![l_some, member])?;
        let null = self.null(sorts[0].clone())?;
        let null_row = self.tm.mk_tuple(&[null])?;
        let inner_has_null = self.member(null_row, inner.clone())?;
        let empty = self.is_empty(inner)?;
        let some_row = self.not(empty)?;
        let l_null_and_rows = self.and(vec![l_null, some_row])?;
        let possible = self.or(vec![l_null_and_rows, inner_has_null])?;
        let t = self.tm.mk_true();
        let some_true = self.some(t)?;
        let unknown = self.null(some_true.sort())?;
        let f = self.tm.mk_false();
        let some_false = self.some(f)?;
        let rest = self.ite(possible, unknown, some_false)?;
        self.ite(matches, some_true, rest)
    }

    /// `(l1, .., ln) IN (subquery)`: TRUE iff some row equals the operand column by column with no
    /// NULL; UNKNOWN iff not, but some row might -- every column equal or one of the pair NULL;
    /// FALSE otherwise. The rows that might are a filter whose predicate reads the operand, which
    /// the solver now decorrelates. Where neither side can be NULL it is plain membership,
    /// `(nullable.some (>= (bag.count (tuple l1 l2) inner) 1))`.
    fn in_subquery_tuple(
        &mut self,
        operand: &[Expr],
        inner: Term,
        query: &Relation,
        inner_base: usize,
        row: &Row,
        schemas: &[Schema],
    ) -> R<Term> {
        let sorts = self.column_sorts(&inner)?;
        let mut ls = Vec::with_capacity(operand.len());
        for (e, sort) in operand.iter().zip(&sorts) {
            let l = self.expr(e, row, schemas)?;
            ls.push(self.as_sort(l, sort)?);
        }
        let tuple = self.tm.mk_tuple(&ls)?;
        let member = self.member(tuple, inner.clone())?;
        let never = operand.iter().enumerate().all(|(c, e)| {
            let op_never = match e {
                Expr::Column { index, .. } => {
                    (*index as usize).checked_sub(row.base).and_then(|i| row.nonnull.get(i).copied()).unwrap_or(false)
                }
                Expr::Literal { value, .. } => value != "NULL",
                _ => false,
            };
            op_never && never_null(query, c, inner_base, schemas)
        });
        if never {
            return self.some(member);
        }
        let mut somes = Vec::with_capacity(ls.len());
        for l in &ls {
            let n = self.is_null(l.clone())?;
            somes.push(self.not(n)?);
        }
        let all_some = self.and(somes)?;
        let matches = self.and(vec![all_some, member])?;
        // rows that might match
        let elem = self.elem_of(&inner)?;
        self.depth += 1;
        let v = self.var(elem, "w")?;
        self.depth -= 1;
        let mut cols = Vec::with_capacity(ls.len());
        for (k, l) in ls.iter().enumerate() {
            let c = self.select(v.clone(), k)?;
            let ln = self.is_null(l.clone())?;
            let cn = self.is_null(c.clone())?;
            let eq = self.tm.mk_term(Kind::Equal, &[l.clone(), c])?;
            cols.push(self.or(vec![ln, cn, eq])?);
        }
        let body = self.and(cols)?;
        let p = self.lambda(&[v], body)?;
        let maybe = self.tm.mk_term(self.filter_kind(), &[p, inner])?;
        let none = self.is_empty(maybe)?;
        let possible = self.not(none)?;
        let t = self.tm.mk_true();
        let some_true = self.some(t)?;
        let unknown = self.null(some_true.sort())?;
        let f = self.tm.mk_false();
        let some_false = self.some(f)?;
        let rest = self.ite(possible, unknown, some_false)?;
        self.ite(matches, some_true, rest)
    }

    // ── Models ────────────────────────────────────────────────────────────────────────────────────

    /// The tables as the model values them.
    fn model(&mut self) -> R<Model> {
        let scanned: Vec<(String, Term)> = self.tables.iter().flatten().cloned().collect();
        let mut tables = Vec::with_capacity(scanned.len());
        for (name, t) in scanned {
            let value = self.solver.get_value(t)?;
            let mut rows = Vec::new();
            self.rows_of(&value, &mut rows)?;
            tables.push(TableModel { name, rows });
        }
        Ok(Model { tables })
    }

    /// Expand a collection value: `bag.empty`, `bag` (a row with a multiplicity) and
    /// `bag.union_disjoint`, or `set.empty`, `set.singleton`, `set.insert` and `set.union` -- the
    /// shapes a value takes. Anything else is not a value, and a bug rather than a limitation.
    fn rows_of(&mut self, value: &Term, rows: &mut Vec<(Vec<Option<String>>, u64)>) -> R<()> {
        match value.kind() {
            Kind::BagEmpty | Kind::SetEmpty => Ok(()),
            Kind::BagMake => {
                let n = value.child(1)?;
                let n: u64 = n.integer_value()?.parse().map_err(|_| Failure::Cvc5(format!("multiplicity {n} in a model")))?;
                if n > 0 {
                    let cells = self.cells_of(&value.child(0)?)?;
                    rows.push((cells, n));
                }
                Ok(())
            }
            Kind::SetSingleton => {
                let cells = self.cells_of(&value.child(0)?)?;
                rows.push((cells, 1));
                Ok(())
            }
            Kind::BagUnionDisjoint | Kind::SetUnion => {
                for i in 0..value.num_children() {
                    self.rows_of(&value.child(i)?, rows)?;
                }
                Ok(())
            }
            Kind::SetInsert => {
                let n = value.num_children();
                for i in 0..n - 1 {
                    let cells = self.cells_of(&value.child(i)?)?;
                    rows.push((cells, 1));
                }
                self.rows_of(&value.child(n - 1)?, rows)
            }
            k => Err(Failure::Cvc5(format!("a model value of kind {}: {value}", cvc5::kind_to_string(k)))),
        }
    }

    fn cells_of(&mut self, tuple: &Term) -> R<Vec<Option<String>>> {
        let fields = tuple.tuple_value()?;
        let mut out = Vec::with_capacity(fields.len());
        for f in fields {
            let is_some = self.is_some(f.clone())?;
            let is_some = self.solver.simplify(is_some, false)?;
            if !is_some.is_boolean_value() {
                return Err(Failure::Cvc5(format!("a nullable field without a value: {f}")));
            }
            if !is_some.boolean_value()? {
                out.push(None);
                continue;
            }
            let v = self.val(f)?;
            let v = self.solver.simplify(v, false)?;
            out.push(Some(scalar_text(&v)));
        }
        Ok(out)
    }
}

enum Side {
    Left,
    Right,
}

fn arith_kind(op: &str) -> Kind {
    match op {
        "+" | "add" => Kind::Add,
        "-" | "sub" => Kind::Sub,
        _ => Kind::Mult,
    }
}

/// The frontend's `q_arith_<op>_<left>_<right>` over INTEGER and REAL operands, as `(op, result
/// is real)`; `None` for any other function, a temporal or opaque operand included.
fn numeric_arith(op: &str) -> Option<(&str, bool)> {
    let rest = op.strip_prefix("q_arith_")?;
    let mut parts = rest.splitn(3, '_');
    let (o, l, r) = (parts.next()?, parts.next()?, parts.next()?);
    let numeric = |t: &str| matches!(t, "integer" | "real");
    if !numeric(l) || !numeric(r) || !matches!(o, "add" | "sub" | "mul" | "div" | "mod") {
        return None;
    }
    Some((o, l == "real" || r == "real"))
}

/// Per column of `rel`, whether it is never NULL ([`never_null`]).
fn never_null_columns(rel: &Relation, base: usize, schemas: &[Schema]) -> Vec<bool> {
    (0..rel.output_types(schemas).len()).map(|c| never_null(rel, c, base, schemas)).collect()
}

/// Whether column `c` of `rel` is never NULL: a NOT NULL column of a table, carried through the
/// operators that copy it (not the padded side of an outer join), a literal other than NULL, or a
/// `COUNT`. `base` is the number of `rel`'s first column in the numbering its expressions use (an
/// enclosing query's columns come first in a subquery). Conservative: anything else may be.
fn never_null(rel: &Relation, c: usize, base: usize, schemas: &[Schema]) -> bool {
    fn expr_never_null(e: &Expr, source: &Relation, base: usize, schemas: &[Schema]) -> bool {
        match e {
            Expr::Column { index, .. } => {
                (*index as usize).checked_sub(base).is_some_and(|i| never_null(source, i, base, schemas))
            }
            Expr::Literal { value, .. } => value != "NULL",
            Expr::Cast { operand, .. } => expr_never_null(operand, source, base, schemas),
            Expr::Call { operator, operand, .. } if operator == "CAST" && operand.len() == 1 => {
                expr_never_null(&operand[0], source, base, schemas)
            }
            // `CASE WHEN TRUE THEN x ..`, which Calcite writes for a column it marks nullable
            Expr::Call { operator, operand, .. } if operator == "CASE" && operand.len() >= 3 && operand.len() % 2 == 1 => {
                is_true(&operand[0]) && expr_never_null(&operand[1], source, base, schemas)
            }
            _ => false,
        }
    }
    match rel {
        Relation::Scan(i) => schemas.get(*i).and_then(|s| s.nullable.get(c)).is_some_and(|n| !*n),
        Relation::Filter { source, .. } | Relation::Sort { source, .. } | Relation::Distinct(source) => {
            never_null(source, c, base, schemas)
        }
        Relation::Project { source, target } => target.get(c).is_some_and(|e| expr_never_null(e, source, base, schemas)),
        Relation::Join { left, right, kind, .. } => {
            let w = left.output_types(schemas).len();
            if c < w {
                matches!(kind, JoinKind::Inner | JoinKind::Left) && never_null(left, c, base, schemas)
            } else {
                matches!(kind, JoinKind::Inner | JoinKind::Right) && never_null(right, c - w, base, schemas)
            }
        }
        Relation::Group { source, keys, function } => {
            if c < keys.len() {
                expr_never_null(&keys[c], source, base, schemas)
            } else {
                function.get(c - keys.len()).is_some_and(|f| f.operator == "COUNT")
            }
        }
        Relation::Values { content, .. } => content
            .iter()
            .all(|row| row.get(c).is_some_and(|e| matches!(e, Expr::Literal { value, .. } if value != "NULL"))),
        Relation::Union(sides) | Relation::Intersect(sides) | Relation::IntersectAll(sides) => {
            never_null(&sides[0], c, base, schemas) && never_null(&sides[1], c, base, schemas)
        }
        Relation::Except(sides) | Relation::ExceptAll(sides) => never_null(&sides[0], c, base, schemas),
    }
}

/// Keys of `rel`'s rows, as sets of its columns no two rows agree on: a table's declared keys, kept by
/// filters and sorts and by a projection that copies their columns, every column after duplicate
/// removal, and a `GROUP BY`'s keys. Conservative: anything else has none known.
fn relation_keys(rel: &Relation, base: usize, schemas: &[Schema]) -> Vec<Vec<usize>> {
    match rel {
        Relation::Scan(i) => schemas.get(*i).map(|s| s.key.clone()).unwrap_or_default(),
        Relation::Filter { source, .. } | Relation::Sort { source, .. } => relation_keys(source, base, schemas),
        Relation::Distinct(source) => {
            let mut keys = relation_keys(source, base, schemas);
            keys.push((0..source.output_types(schemas).len()).collect());
            keys
        }
        Relation::Project { source, target } => {
            let mut keys = Vec::new();
            for key in relation_keys(source, base, schemas) {
                let mut cols = Vec::new();
                for c in &key {
                    let at = target.iter().position(
                        |e| matches!(e, Expr::Column { index, .. } if (*index as usize).checked_sub(base) == Some(*c)),
                    );
                    match at {
                        Some(p) => cols.push(p),
                        None => break,
                    }
                }
                if cols.len() == key.len() {
                    keys.push(cols);
                }
            }
            keys
        }
        Relation::Group { keys, .. } if !keys.is_empty() => vec![(0..keys.len()).collect()],
        _ => Vec::new(),
    }
}

/// Whether column `c` of `rel` is NULL in every row: a NULL literal (or a cast of one) in a VALUES
/// or a projection, carried through filters, sorts, duplicate removal, projections that copy it,
/// joins and set operations. Conservative: anything else is not known to be.
fn always_null(rel: &Relation, c: usize, schemas: &[Schema]) -> bool {
    fn null_literal(e: &Expr) -> bool {
        match e {
            Expr::Literal { value, .. } => value == "NULL",
            Expr::Cast { operand, .. } => null_literal(operand),
            Expr::Call { operator, operand, .. } if operator == "CAST" && operand.len() == 1 => {
                null_literal(&operand[0])
            }
            // A strict function is NULL as soon as an argument is: `q_cast_varchar_20(NULL)`.
            Expr::Call { operator, operand, .. } if strict_function(operator) => operand.iter().any(null_literal),
            _ => false,
        }
    }
    match rel {
        Relation::Values { content, .. } => content.iter().all(|row| row.get(c).is_some_and(null_literal)),
        Relation::Filter { source, .. } | Relation::Sort { source, .. } | Relation::Distinct(source) => {
            always_null(source, c, schemas)
        }
        Relation::Project { source, target } => match target.get(c) {
            Some(e) if null_literal(e) => true,
            Some(Expr::Column { index, .. }) => always_null(source, *index as usize, schemas),
            _ => false,
        },
        Relation::Join { left, right, .. } => {
            let w = left.output_types(schemas).len();
            if c < w {
                always_null(left, c, schemas)
            } else {
                always_null(right, c - w, schemas)
            }
        }
        Relation::Union(sides) | Relation::Intersect(sides) | Relation::IntersectAll(sides) => {
            always_null(&sides[0], c, schemas) && always_null(&sides[1], c, schemas)
        }
        Relation::Except(sides) | Relation::ExceptAll(sides) => always_null(&sides[0], c, schemas),
        Relation::Group { source, keys, .. } => match keys.get(c) {
            Some(e) if null_literal(e) => true,
            Some(Expr::Column { index, .. }) => always_null(source, *index as usize, schemas),
            _ => false,
        },
        _ => false,
    }
}

fn is_true(e: &Expr) -> bool {
    matches!(e, Expr::Literal { value, ty: Type::Boolean } if value.eq_ignore_ascii_case("true"))
}

/// The functions the encoding does not interpret but knows to be null exactly when an argument is
/// (strict, and total on non-null input), as `sqleq-solver` lists them: the frontend's named
/// conversions, casts and temporal operators, and a handful of Postgres functions.
fn strict_function(op: &str) -> bool {
    matches!(
        op,
        "UPPER" | "LOWER" | "ABS" | "SUBSTRING" | "TRIM_BOTH" | "TRIM_LEADING" | "TRIM_TRAILING" | "DATE" | "DATE_TRUNC"
            | "TO_CHAR" | "TO_TIMESTAMP" | "CARDINALITY" | "NLEVEL" | "LIKE" | "||"
    ) || op.strip_prefix("QCAST").is_some_and(|k| !k.is_empty() && k.bytes().all(|b| b.is_ascii_digit()))
        || op.starts_with("q_conv_")
        || op.starts_with("q_arith_")
        || op.starts_with("q_cast_")
}

/// The name of an uninterpreted symbol for `op` over `operand`'s IR types, with result type `ty`:
/// Postgres resolves a function by its name and argument types, so the symbol carries them.
fn typed_symbol(op: &str, operand: &[Expr], ty: Type) -> String {
    let args: Vec<&str> = operand.iter().map(|e| e.ty().name()).collect();
    format!("{op}({})->{}", args.join(","), ty.name())
}

/// A model scalar as text: integers and reals as cvc5 prints them, strings quoted, booleans as
/// `true`/`false`, and a value of an uninterpreted sort by its name.
fn scalar_text(v: &Term) -> String {
    if v.is_integer_value() {
        return v.integer_value().unwrap_or_else(|_| v.to_string());
    }
    if v.is_real_value() {
        return v.real_value().unwrap_or_else(|_| v.to_string());
    }
    if v.is_boolean_value() {
        return v.boolean_value().map(|b| b.to_string()).unwrap_or_else(|_| v.to_string());
    }
    if v.is_string_value() {
        let chars: String = v
            .string_value()
            .map(|cs| cs.into_iter().filter_map(char::from_u32).collect())
            .unwrap_or_else(|_| v.to_string());
        return format!("'{chars}'");
    }
    v.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn refusals_spell_like_sqleq_solver() {
        assert_eq!(Failure::from(TranslateError::Correlated).to_string(), "NOTRANS correlated");
        assert_eq!(unsupported("x").to_string(), "NOTRANS unsupported:x");
    }

    #[test]
    fn a_model_summarizes_like_a_fuzz_counterexample() {
        let m = Model {
            tables: vec![
                TableModel { name: "emp".into(), rows: vec![(vec![Some("1".into()), None], 1), (vec![Some("2".into()), Some("'x'".into())], 3)] },
                TableModel { name: "dept".into(), rows: vec![] },
            ],
        };
        assert_eq!(m.summary(), "emp=[(1,NULL); (2,'x')×3] dept=[]");
    }

    #[test]
    fn strict_functions_are_the_frontends_named_ones() {
        assert!(strict_function("q_conv_date_timestamp"));
        assert!(strict_function("QCAST3"));
        assert!(!strict_function("QCAST"));
        assert!(!strict_function("POWER"));
        assert!(!strict_function("QP1"));
    }

    #[test]
    fn the_frontends_numeric_arithmetic_is_recognised() {
        assert_eq!(numeric_arith("q_arith_div_integer_integer"), Some(("div", false)));
        assert_eq!(numeric_arith("q_arith_add_real_integer"), Some(("add", true)));
        assert_eq!(numeric_arith("q_arith_add_date_integer"), None);
        assert_eq!(numeric_arith("q_conv_date_timestamp"), None);
    }

    #[test]
    fn symbols_carry_their_types() {
        let e = Expr::Column { index: 0, ty: Type::Integer };
        assert_eq!(typed_symbol("POWER", &[e.clone(), e], Type::Real), "POWER(INTEGER,INTEGER)->REAL");
    }
}
