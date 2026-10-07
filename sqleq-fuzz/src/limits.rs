// Unless explicitly stated otherwise all files in this repository are licensed under the
// Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/).
// Copyright 2026-Present Datadog, Inc.

//! `LIMIT`, `OFFSET` and `FETCH`: which of them can cut rows, and whether the rows they keep are
//! determined.
//!
//! A cut over rows whose order leaves ties keeps an arbitrary choice among the tied rows, so two
//! runs of the *same* query can return different bags. [`crate::pair::test_pair`] then trusts only a
//! difference in cardinality, which no tie-break can change. Two things narrow that rule back down:
//!
//! * a count that is a bare `$N` and nothing else is bound so that it cuts nothing -- a large
//!   `LIMIT`, an `OFFSET` of 0 -- and a literal `OFFSET 0`, `LIMIT ALL` or `LIMIT NULL` cuts nothing
//!   either;
//! * a cut whose `ORDER BY` is a *total* order on the rows it orders keeps a determined set of rows,
//!   so the bag it returns is a function of the instance and can be compared whole ([`Cut::total`]).
//!
//! Everything is read off the Postgres parse, so `LIMIT (1)`, `FETCH FIRST ROW ONLY` and
//! `LIMIT ($1)` are seen for what they are. A statement that does not parse is scanned for the
//! keywords instead and every cut in it is taken to be nondeterministic.

use std::collections::HashSet;
use std::ops::ControlFlow;

use sqlparser::ast::{
    BinaryOperator, Distinct, Expr, GroupByExpr, JoinConstraint, JoinOperator, LimitClause,
    ObjectName, ObjectNamePart, OrderByKind, Query, Select, SelectItem, SetExpr, Statement,
    TableFactor, Value, Visit, Visitor,
};
use sqlparser::dialect::PostgreSqlDialect;
use sqlparser::keywords::Keyword;
use sqlparser::parser::Parser;
use sqlparser::tokenizer::Token;

use crate::lex::significant;
use crate::schema::{resolve, Schema, Table};

/// Which clause a `$N` count belongs to.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Count {
    /// `LIMIT $N` or `FETCH FIRST $N ROWS`: cuts nothing when bound large.
    Limit,
    /// `OFFSET $N`: cuts nothing when bound to 0.
    Offset,
}

/// One query level that carries a `LIMIT`, `OFFSET` or `FETCH`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Cut {
    /// The counts at this level that are a bare `$N` (parentheses allowed), with their clause.
    pub params: Vec<(u32, Count)>,
    /// A count that cuts whatever is bound: any literal or expression other than `0`, `ALL` or
    /// `NULL`, and `FETCH FIRST ROW ONLY`, whose count is an implicit 1.
    pub fixed: bool,
    /// Whether this level's `ORDER BY` is a total order on its rows, so the rows any count keeps are
    /// determined ([`is_total`]).
    pub total: bool,
}

/// The cuts in `sql`, one per query level that has one, subqueries and CTEs included.
pub fn cuts(sql: &str, schema: &Schema) -> Vec<Cut> {
    let Ok(stmts) = Parser::parse_sql(&PostgreSqlDialect {}, sql) else {
        return unparsed(sql);
    };
    let mut ctes = CteNames::default();
    for st in &stmts {
        let _ = Statement::visit(st, &mut ctes);
    }
    let mut finder = Finder {
        schema,
        ctes: ctes.0,
        out: Vec::new(),
    };
    for st in &stmts {
        let _ = Statement::visit(st, &mut finder);
    }
    finder.out
}

/// A statement the parser rejected: any `LIMIT`/`OFFSET`/`FETCH` keyword is one cut that cuts and is
/// not known to be ordered, which leaves only the cardinality comparison -- sound whatever the
/// clause turns out to say.
fn unparsed(sql: &str) -> Vec<Cut> {
    let has = significant(sql)
        .map(|toks| {
            toks.iter().any(|t| {
                matches!(&t.token, Token::Word(w)
                    if w.quote_style.is_none()
                        && matches!(w.keyword, Keyword::LIMIT | Keyword::OFFSET | Keyword::FETCH))
            })
        })
        .unwrap_or_else(|| {
            let l = sql.to_lowercase();
            l.contains("limit") || l.contains("offset") || l.contains("fetch")
        });
    if has {
        vec![Cut {
            params: Vec::new(),
            fixed: true,
            total: false,
        }]
    } else {
        Vec::new()
    }
}

/// Every CTE name declared anywhere in the statement: a `FROM` entry spelled like one may be the CTE
/// rather than the table, and only a table's keys are known.
#[derive(Default)]
struct CteNames(HashSet<String>);

impl Visitor for CteNames {
    type Break = ();
    fn pre_visit_query(&mut self, q: &Query) -> ControlFlow<()> {
        if let Some(w) = &q.with {
            for cte in &w.cte_tables {
                self.0.insert(cte.alias.name.value.to_lowercase());
            }
        }
        ControlFlow::Continue(())
    }
}

struct Finder<'a> {
    schema: &'a Schema,
    ctes: HashSet<String>,
    out: Vec<Cut>,
}

/// What a count expression says about the rows it can cut.
enum CountExpr {
    Param(u32),
    /// `0`, `NULL`: nothing is cut.
    Nothing,
    Other,
}

fn count_of(e: &Expr) -> CountExpr {
    match e {
        Expr::Nested(inner) => count_of(inner),
        Expr::Value(v) => match &v.value {
            Value::Placeholder(p) => match p.strip_prefix('$').and_then(|d| d.parse().ok()) {
                Some(n) => CountExpr::Param(n),
                None => CountExpr::Other,
            },
            Value::Null => CountExpr::Nothing,
            Value::Number(n, _) if n.parse::<f64>().is_ok_and(|x| x == 0.0) => CountExpr::Nothing,
            _ => CountExpr::Other,
        },
        _ => CountExpr::Other,
    }
}

impl Visitor for Finder<'_> {
    type Break = ();

    fn pre_visit_query(&mut self, q: &Query) -> ControlFlow<()> {
        let mut cut = Cut::default();
        let add = |e: &Expr, kind: Count, cut: &mut Cut| match count_of(e) {
            CountExpr::Param(n) => cut.params.push((n, kind)),
            CountExpr::Nothing => {}
            CountExpr::Other => cut.fixed = true,
        };
        match &q.limit_clause {
            Some(LimitClause::LimitOffset { limit, offset, .. }) => {
                if let Some(l) = limit {
                    add(l, Count::Limit, &mut cut);
                }
                if let Some(o) = offset {
                    add(&o.value, Count::Offset, &mut cut);
                }
            }
            Some(LimitClause::OffsetCommaLimit { offset, limit }) => {
                add(limit, Count::Limit, &mut cut);
                add(offset, Count::Offset, &mut cut);
            }
            None => {}
        }
        if let Some(f) = &q.fetch {
            match &f.quantity {
                Some(e) => add(e, Count::Limit, &mut cut),
                None => cut.fixed = true, // `FETCH FIRST ROW ONLY` keeps one row
            }
        }
        if cut.fixed || !cut.params.is_empty() {
            cut.total = is_total(q, self.schema, &self.ctes);
            self.out.push(cut);
        }
        ControlFlow::Continue(())
    }
}

/// Functions that can return several rows from the select list. One of them in the projection
/// multiplies a source row into rows that share every `ORDER BY` value, so the order is not total
/// however keyed the tables are. A function DuckDB has no name for cannot run at all, so the list
/// only needs DuckDB's own and the shims' (`crate::shim`), plus Postgres's for good measure.
const SET_RETURNING: &[&str] = &[
    "unnest",
    "generate_series",
    "generate_subscripts",
    "range",
    "json_array_elements",
    "json_array_elements_text",
    "jsonb_array_elements",
    "jsonb_array_elements_text",
    "json_object_keys",
    "jsonb_object_keys",
    "json_each",
    "json_each_text",
    "jsonb_each",
    "jsonb_each_text",
    "json_to_recordset",
    "jsonb_to_recordset",
    "json_populate_recordset",
    "jsonb_populate_recordset",
    "jsonb_path_query",
    "regexp_matches",
    "regexp_split_to_table",
    "string_to_table",
];

#[derive(Default)]
struct HasSetReturning(bool);

impl Visitor for HasSetReturning {
    type Break = ();
    fn pre_visit_expr(&mut self, e: &Expr) -> ControlFlow<()> {
        if let Expr::Function(f) = e {
            let name = f
                .name
                .0
                .last()
                .and_then(|p| match p {
                    ObjectNamePart::Identifier(id) => Some(id.value.to_lowercase()),
                    _ => None,
                })
                .unwrap_or_default();
            if SET_RETURNING.contains(&name.as_str()) {
                self.0 = true;
                return ControlFlow::Break(());
            }
        }
        ControlFlow::Continue(())
    }
}

/// One table in a `FROM` clause: the name it is visible under, and its declaration.
struct Inst<'a> {
    name: String,
    table: &'a Table,
}

/// A column of an instance: (instance index, column index).
type Col = (usize, usize);

/// Whether `q`'s `ORDER BY` is a total order on its rows: no two of them can tie.
///
/// A static, deliberately narrow check. The level must be a plain `SELECT` (no set operation, no
/// `DISTINCT ON`, no set-returning function in its projection) over base tables joined by inner or
/// cross joins, every one of them declared in the DDL. Then a row is one combination of one row of
/// each table, and two rows that agree on every `ORDER BY` value are the same row if those values
/// *determine* a row of every table. That is computed as a closure, starting from the `ORDER BY`
/// columns (an output alias or position counts as the column it names) and from the columns an
/// equality conjunct pins to a constant, a parameter or an outer column:
///
/// * an equality conjunct `x = y` between two columns, in `WHERE` or an inner join's `ON`/`USING`,
///   makes either one determine the other;
/// * a UNIQUE or PRIMARY KEY whose columns are all determined and cannot be NULL on these rows --
///   declared NOT NULL, or held by an equality or `IS NOT NULL` conjunct, which no NULL satisfies --
///   determines that table's row, and with it every column of the table.
///
/// The order is total when every table's row is determined -- or, for a `GROUP BY` level, when every
/// grouping column is, since a group is identified by those. Every rule only ever adds columns a
/// conjunct or a key really fixes, so an answer of `true` is a proof; anything outside the narrow
/// shape answers `false`, which keeps the cardinality-only comparison.
pub fn is_total(q: &Query, schema: &Schema, ctes: &HashSet<String>) -> bool {
    let Some(OrderByKind::Expressions(items)) = q.order_by.as_ref().map(|o| &o.kind) else {
        return false;
    };
    let SetExpr::Select(sel) = &*q.body else {
        return false;
    };
    if matches!(sel.distinct, Some(Distinct::On(_))) {
        return false;
    }
    let mut srf = HasSetReturning::default();
    for item in &sel.projection {
        let _ = item.visit(&mut srf);
    }
    if srf.0 {
        return false;
    }
    let Some((insts, mut conjuncts, usings)) = from_clause(sel, schema, ctes) else {
        return false;
    };
    if let Some(w) = &sel.selection {
        split_and(w, &mut conjuncts);
    }

    let mut known: HashSet<Col> = HashSet::new();
    let mut nonnull: HashSet<Col> = usings.iter().flat_map(|(x, y)| [*x, *y]).collect();
    let mut edges: Vec<(Col, Col)> = usings;
    for c in &conjuncts {
        match strip(c) {
            Expr::BinaryOp {
                left,
                op: BinaryOperator::Eq,
                right,
            } => match (column(left, &insts), column(right, &insts)) {
                (Some(x), Some(y)) => {
                    edges.push((x, y));
                    nonnull.extend([x, y]);
                }
                (Some(x), None) if is_constant(right, &insts) => {
                    known.insert(x);
                    nonnull.insert(x);
                }
                (None, Some(y)) if is_constant(left, &insts) => {
                    known.insert(y);
                    nonnull.insert(y);
                }
                _ => {}
            },
            Expr::IsNotNull(e) => {
                if let Some(x) = column(e, &insts) {
                    nonnull.insert(x);
                }
            }
            _ => {}
        }
    }
    for item in items {
        if let Some(c) = order_column(&item.expr, sel, &insts) {
            known.insert(c);
        }
    }

    // The closure. Each pass adds at least one column or stops, so it terminates.
    let mut whole: HashSet<usize> = HashSet::new();
    loop {
        let before = (known.len(), whole.len());
        for (x, y) in &edges {
            if known.contains(x) {
                known.insert(*y);
            }
            if known.contains(y) {
                known.insert(*x);
            }
        }
        for (i, inst) in insts.iter().enumerate() {
            if whole.contains(&i) {
                continue;
            }
            let keyed = inst.table.keys.iter().any(|key| {
                !key.is_empty()
                    && key.iter().all(|name| {
                        inst.table
                            .cols
                            .iter()
                            .position(|c| &c.name == name)
                            .is_some_and(|j| {
                                known.contains(&(i, j))
                                    && (inst.table.cols[j].notnull || nonnull.contains(&(i, j)))
                            })
                    })
            });
            if keyed {
                whole.insert(i);
                known.extend((0..inst.table.cols.len()).map(|j| (i, j)));
            }
        }
        if (known.len(), whole.len()) == before {
            break;
        }
    }

    match &sel.group_by {
        GroupByExpr::Expressions(exprs, mods) if !exprs.is_empty() => {
            mods.is_empty()
                && exprs
                    .iter()
                    .all(|e| column(e, &insts).is_some_and(|c| known.contains(&c)))
        }
        GroupByExpr::Expressions(..) => whole.len() == insts.len(),
        GroupByExpr::All(_) => false,
    }
}

/// The tables of a `FROM` clause, the conjuncts its inner joins' `ON` clauses state, and the column
/// equalities their `USING` lists state; `None` if it holds anything but base tables under inner or
/// cross joins.
#[allow(clippy::type_complexity)]
fn from_clause<'a>(
    sel: &'a Select,
    schema: &'a Schema,
    ctes: &HashSet<String>,
) -> Option<(Vec<Inst<'a>>, Vec<&'a Expr>, Vec<(Col, Col)>)> {
    let mut insts: Vec<Inst> = Vec::new();
    let mut conjuncts: Vec<&Expr> = Vec::new();
    let mut usings: Vec<(usize, String)> = Vec::new();
    for twj in &sel.from {
        insts.push(instance(&twj.relation, schema, ctes)?);
        for j in &twj.joins {
            let constraint = match &j.join_operator {
                JoinOperator::Join(c) | JoinOperator::Inner(c) | JoinOperator::CrossJoin(c) => c,
                _ => return None,
            };
            insts.push(instance(&j.relation, schema, ctes)?);
            match constraint {
                JoinConstraint::On(e) => split_and(e, &mut conjuncts),
                JoinConstraint::Using(cols) => {
                    for c in cols {
                        let name = c.0.last().and_then(|p| match p {
                            ObjectNamePart::Identifier(id) => Some(id.value.to_lowercase()),
                            _ => None,
                        })?;
                        usings.push((insts.len() - 1, name));
                    }
                }
                JoinConstraint::None => {}
                JoinConstraint::Natural => return None,
            }
        }
    }
    let mut names: HashSet<&str> = HashSet::new();
    if !insts.iter().all(|i| names.insert(i.name.as_str())) {
        return None; // the same name twice is a query Postgres rejects; do not reason about it
    }
    // `USING (c)` is `left.c = right.c`, for the one earlier table that has `c`; where none or
    // several do, the equality is not used, which only leaves the closure smaller.
    let col_of = |i: usize, name: &str| insts[i].table.cols.iter().position(|c| c.name == name);
    let mut eqs: Vec<(Col, Col)> = Vec::new();
    for (right, name) in usings {
        let lefts: Vec<Col> = (0..right)
            .filter_map(|i| col_of(i, &name).map(|j| (i, j)))
            .collect();
        if let ([left], Some(j)) = (lefts.as_slice(), col_of(right, &name)) {
            eqs.push((*left, (right, j)));
        }
    }
    Some((insts, conjuncts, eqs))
}

/// A `FROM` entry, if it is a table the DDL declares and not a CTE, a function or a subquery.
fn instance<'a>(f: &TableFactor, schema: &'a Schema, ctes: &HashSet<String>) -> Option<Inst<'a>> {
    let TableFactor::Table {
        name,
        alias,
        args: None,
        sample: None,
        ..
    } = f
    else {
        return None;
    };
    let parts = parts_of(name);
    if parts.len() == 1 && ctes.contains(&parts[0]) {
        return None;
    }
    let key = resolve(schema, &parts)?;
    let table = &schema[key];
    if alias.as_ref().is_some_and(|a| !a.columns.is_empty()) {
        return None; // renamed columns: the names below would not be the table's
    }
    let name = match alias {
        Some(a) => a.name.value.to_lowercase(),
        None => parts.last()?.clone(),
    };
    Some(Inst { name, table })
}

fn parts_of(n: &ObjectName) -> Vec<String> {
    n.0.iter()
        .filter_map(|p| match p {
            ObjectNamePart::Identifier(id) => Some(id.value.to_lowercase()),
            _ => None,
        })
        .collect()
}

fn strip(e: &Expr) -> &Expr {
    match e {
        Expr::Nested(inner) => strip(inner),
        e => e,
    }
}

fn split_and<'a>(e: &'a Expr, out: &mut Vec<&'a Expr>) {
    match strip(e) {
        Expr::BinaryOp {
            left,
            op: BinaryOperator::And,
            right,
        } => {
            split_and(left, out);
            split_and(right, out);
        }
        e => out.push(e),
    }
}

/// The column `e` names among `insts`: a qualified name through its qualifier, an unqualified one
/// only if exactly one table has it.
fn column(e: &Expr, insts: &[Inst]) -> Option<Col> {
    let find = |i: usize, name: &str| {
        insts[i]
            .table
            .cols
            .iter()
            .position(|c| c.name == name)
            .map(|j| (i, j))
    };
    match strip(e) {
        Expr::Identifier(id) => {
            let name = id.value.to_lowercase();
            let hits: Vec<Col> = (0..insts.len()).filter_map(|i| find(i, &name)).collect();
            (hits.len() == 1).then(|| hits[0])
        }
        Expr::CompoundIdentifier(parts) if parts.len() >= 2 => {
            let name = parts[parts.len() - 1].value.to_lowercase();
            let qual = parts[parts.len() - 2].value.to_lowercase();
            let i = insts.iter().position(|inst| inst.name == qual)?;
            find(i, &name)
        }
        _ => None,
    }
}

/// Whether `e` is the same value on every row of this level: a non-NULL literal, a placeholder, or
/// a column qualified by a name that is not a table here (an outer reference, fixed for each
/// evaluation of a correlated subquery).
fn is_constant(e: &Expr, insts: &[Inst]) -> bool {
    match strip(e) {
        Expr::Value(v) => !matches!(v.value, Value::Null),
        Expr::CompoundIdentifier(parts) if parts.len() >= 2 => {
            let qual = parts[parts.len() - 2].value.to_lowercase();
            !insts.iter().any(|i| i.name == qual)
        }
        _ => false,
    }
}

/// The column an `ORDER BY` item sorts by. Postgres reads a bare name as an output column first and a
/// number as a position in the select list, so both are looked up there before the `FROM` tables.
fn order_column(e: &Expr, sel: &Select, insts: &[Inst]) -> Option<Col> {
    let projected = |item: &SelectItem| match item {
        SelectItem::UnnamedExpr(x) | SelectItem::ExprWithAlias { expr: x, .. } => Some(x.clone()),
        _ => None,
    };
    match strip(e) {
        Expr::Identifier(id) => {
            let name = id.value.to_lowercase();
            for item in &sel.projection {
                if let SelectItem::ExprWithAlias { expr, alias } = item {
                    if alias.value.to_lowercase() == name {
                        return column(expr, insts);
                    }
                }
            }
            column(e, insts)
        }
        Expr::Value(v) => match &v.value {
            Value::Number(n, _) => {
                let k: usize = n.parse().ok()?;
                let item = sel.projection.get(k.checked_sub(1)?)?;
                column(&projected(item)?, insts)
            }
            _ => None,
        },
        _ => column(e, insts),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::parse_schema;

    fn total(sql: &str, ddl: &str) -> Vec<bool> {
        cuts(sql, &parse_schema(ddl)).iter().map(|c| c.total).collect()
    }

    const TU: &str = "create table t (id int primary key, x int not null unique, n int, \
                      w int unique); create table u (id int primary key, y int not null unique)";

    #[test]
    fn every_spelling_of_a_cut_is_found() {
        let s = parse_schema(TU);
        for sql in [
            "SELECT id FROM t ORDER BY n LIMIT 1",
            "SELECT id FROM t ORDER BY n LIMIT (1)",
            "SELECT id FROM t ORDER BY n FETCH FIRST ROW ONLY",
            "SELECT id FROM t ORDER BY n FETCH NEXT 2 ROWS ONLY",
            "SELECT id FROM t ORDER BY n OFFSET 1",
            "SELECT id FROM t ORDER BY n LIMIT $1 + 1",
        ] {
            let c = cuts(sql, &s);
            assert_eq!(c.len(), 1, "{sql}");
            assert!(c[0].fixed && c[0].params.is_empty(), "{sql}: {c:?}");
        }
        for (sql, want) in [
            ("SELECT id FROM t LIMIT $1", vec![(1, Count::Limit)]),
            ("SELECT id FROM t LIMIT ($1)", vec![(1, Count::Limit)]),
            ("SELECT id FROM t OFFSET $2", vec![(2, Count::Offset)]),
            (
                "SELECT id FROM t LIMIT $1 OFFSET $2",
                vec![(1, Count::Limit), (2, Count::Offset)],
            ),
            ("SELECT id FROM t FETCH FIRST $3 ROWS ONLY", vec![(3, Count::Limit)]),
        ] {
            let c = cuts(sql, &s);
            assert_eq!(c.len(), 1, "{sql}");
            assert!(!c[0].fixed, "{sql}");
            assert_eq!(c[0].params, want, "{sql}");
        }
        // Clauses that cut nothing.
        for sql in [
            "SELECT id FROM t OFFSET 0",
            "SELECT id FROM t LIMIT ALL",
            "SELECT id FROM t LIMIT NULL",
            "SELECT id FROM t",
        ] {
            assert!(cuts(sql, &s).is_empty(), "{sql}: {:?}", cuts(sql, &s));
        }
        // Subqueries each carry their own.
        assert_eq!(
            cuts(
                "SELECT id FROM t WHERE id IN (SELECT id FROM u ORDER BY id LIMIT 1) LIMIT 2",
                &s
            )
            .len(),
            2
        );
    }

    #[test]
    fn a_key_in_the_order_by_is_total() {
        assert_eq!(total("SELECT n FROM t ORDER BY id LIMIT 1", TU), [true]);
        assert_eq!(total("SELECT n FROM t ORDER BY n, x LIMIT 1", TU), [true]);
        assert_eq!(total("SELECT id AS k FROM t ORDER BY k LIMIT 1", TU), [true]);
        assert_eq!(total("SELECT n, id FROM t ORDER BY 2 LIMIT 1", TU), [true]);
        assert_eq!(total("SELECT n FROM t AS a ORDER BY a.id LIMIT 1", TU), [true]);
        // The join from the issue: x and y are unique and NOT NULL, so t.id determines the u row.
        assert_eq!(
            total(
                "SELECT t.id, u.id FROM t JOIN u ON t.x = u.y ORDER BY t.id LIMIT 1",
                TU
            ),
            [true]
        );
        // A nullable unique key held by an equality: no NULL satisfies it.
        assert_eq!(
            total("SELECT n FROM t WHERE w = $1 ORDER BY n LIMIT 1", TU),
            [true]
        );
        assert_eq!(
            total("SELECT n FROM t WHERE w IS NOT NULL ORDER BY w LIMIT 1", TU),
            [true]
        );
    }

    #[test]
    fn anything_short_of_a_key_is_not() {
        for sql in [
            "SELECT n FROM t ORDER BY n LIMIT 1",
            // A nullable unique column: two NULLs tie.
            "SELECT n FROM t ORDER BY w LIMIT 1",
            // Only one side of a join keyed, with nothing tying the other to it.
            "SELECT t.id FROM t, u ORDER BY t.id LIMIT 1",
            "SELECT t.id FROM t LEFT JOIN u ON t.x = u.y ORDER BY t.id LIMIT 1",
            // A set-returning function repeats the key.
            "SELECT id, unnest(ARRAY[1, 2]) FROM t ORDER BY id LIMIT 1",
            "SELECT DISTINCT ON (n) n, id FROM t ORDER BY n, id LIMIT 1",
            "SELECT id FROM t UNION SELECT id FROM u ORDER BY id LIMIT 1",
            "SELECT id FROM (SELECT id FROM t) s ORDER BY id LIMIT 1",
            // A CTE named like a table is the CTE, whose keys are unknown.
            "WITH t AS (SELECT 1 AS id) SELECT id FROM t ORDER BY id LIMIT 1",
            // An expression is not a column, even over one.
            "SELECT n FROM t ORDER BY id + 0 LIMIT 1",
            "SELECT n FROM t LIMIT 1",
        ] {
            assert_eq!(total(sql, TU), [false], "{sql}");
        }
    }

    #[test]
    fn a_grouped_level_is_total_on_its_grouping_columns() {
        assert_eq!(
            total("SELECT n, count(*) FROM t GROUP BY n ORDER BY n LIMIT 1", TU),
            [true]
        );
        assert_eq!(
            total("SELECT n, count(*) FROM t GROUP BY n ORDER BY 2 LIMIT 1", TU),
            [false]
        );
    }

    #[test]
    fn an_unparsed_statement_with_a_cut_is_never_total() {
        let c = cuts("SELECT id FROM t ORDER BY id LIMIT 1 ;;; nonsense (", &parse_schema(TU));
        assert_eq!(
            c,
            vec![Cut {
                params: vec![],
                fixed: true,
                total: false
            }]
        );
    }
}
