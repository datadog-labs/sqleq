// Unless explicitly stated otherwise all files in this repository are licensed under the
// Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/).
// Copyright 2026-Present Datadog, Inc.

//! Which column of which table each parameter is compared with, for the row trials.
//!
//! The other trials link a parameter to a column by a pattern over the statement's text
//! (`patterns::param_cols`): a bare word beside `$N`, looked up by name among every table's
//! columns, the first table that has one winning. That is enough to draw a value toward what some
//! column of that name holds. A row trial binds the parameters compared with one table's columns
//! to the values of one of its rows (`pg::test_pair`), so it has to know the table, and a pattern
//! over text does not: it reads no quoted name (`"tenant" = $1`, which is what an ORM and pg_dump
//! write), and it sends `o.id = $1` to whichever table declares an `id` first.
//!
//! So the row trials read the parsed statements, as Postgres resolves a column reference:
//!
//! * **A qualified reference** goes to the table its qualifier names in the nearest enclosing
//!   `FROM`, by alias where there is one. A qualifier that names a derived table, a CTE or a
//!   function links nothing.
//! * **A bare reference** goes to the one table of the nearest enclosing `FROM` that has the
//!   column. Where that `FROM` also holds something whose columns are not known (a derived table,
//!   a CTE, a function), or two of its tables have the column, it links nothing: guessing a table
//!   is the mistake this replaces.
//! * **A statement that does not parse** links nothing, and gets no row trial.
//!
//! Nothing here can make a row trial report a difference that is not one: a counterexample is two
//! runs of the pair on one database, whatever the parameters were bound from. A link only decides
//! how likely a trial is to select a row.

use std::collections::{BTreeSet, HashMap};
use std::ops::ControlFlow;

use sqlparser::ast::{
    BinaryOperator, Expr, FromTable, InTarget, ObjectName, ObjectNamePart, Query, Select, Statement,
    TableFactor, TableWithJoins, UpdateTableFromKind, Value, Visit, Visitor,
};
use sqlparser::dialect::PostgreSqlDialect;
use sqlparser::parser::Parser;

use crate::fuzz::schema::{resolve, Schema};

/// One comparison of a parameter with a column of a table the statements read.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Link {
    pub param: u32,
    /// The table's key in the schema.
    pub table: String,
    /// The column's index among the table's columns.
    pub col: usize,
    /// Whether the comparison holds where the parameter is the column's own value: `=`, `>=`,
    /// `<=`, `IS NOT DISTINCT FROM`, and a `BETWEEN`, `IN`, `= ANY` or `LIKE` that is not negated.
    /// Under `<`, `>`, `<>` or `IS DISTINCT FROM` the row's own value is the one value that
    /// excludes the row, which a row trial binds only some of the time.
    pub selects: bool,
    /// Whether the column is read through a cast (`created_at::date = $1`), so the parameter need
    /// not be of the column's type.
    pub cast: bool,
    /// Whether the parameter is the array of `col = ANY($N)`.
    pub array: bool,
}

/// Every [`Link`] the two statements make, each once.
pub fn row_links(a: &str, b: &str, schema: &Schema) -> Vec<Link> {
    let mut links = Links { schema, frames: Vec::new(), ctes: Vec::new(), out: BTreeSet::new() };
    for sql in [a, b] {
        if let Ok(parsed) = Parser::parse_sql(&PostgreSqlDialect {}, sql) {
            for st in &parsed {
                let _ = st.visit(&mut links);
            }
        }
    }
    links.out.into_iter().collect()
}

/// What one `FROM` (or the target and `FROM` of an `UPDATE` or `DELETE`) brings into scope.
#[derive(Default)]
struct Frame {
    /// The name a reference qualifies by -> the base table it stands for, or nothing for a
    /// derived table, a CTE or a function.
    names: HashMap<String, Option<String>>,
    /// Whether some item's columns are not known, so a bare name may be its.
    opaque: bool,
}

struct Links<'a> {
    schema: &'a Schema,
    frames: Vec<Frame>,
    /// The names each enclosing query's `WITH` binds.
    ctes: Vec<BTreeSet<String>>,
    out: BTreeSet<Link>,
}

fn lower(name: &ObjectName) -> Option<Vec<String>> {
    name.0
        .iter()
        .map(|p| match p {
            ObjectNamePart::Identifier(id) => Some(id.value.to_lowercase()),
            _ => None,
        })
        .collect()
}

impl Links<'_> {
    fn add(&self, frame: &mut Frame, item: &TableWithJoins) {
        self.add_factor(frame, &item.relation);
        for join in &item.joins {
            self.add_factor(frame, &join.relation);
        }
    }

    fn add_factor(&self, frame: &mut Frame, factor: &TableFactor) {
        match factor {
            TableFactor::Table { name, alias, args, .. } => {
                let parts = lower(name);
                let cte = parts.as_ref().is_some_and(|p| p.len() == 1 && self.ctes.iter().any(|w| w.contains(&p[0])));
                // An alias that renames the columns leaves none of the table's names in scope.
                let renamed = alias.as_ref().is_some_and(|a| !a.columns.is_empty());
                let table = match &parts {
                    Some(parts) if args.is_none() && !cte && !renamed => resolve(self.schema, parts).cloned(),
                    _ => None,
                };
                let key = match alias {
                    Some(a) => Some(a.name.value.to_lowercase()),
                    None => parts.and_then(|mut p| p.pop()),
                };
                frame.opaque |= table.is_none();
                if let Some(key) = key {
                    frame.names.insert(key, table);
                }
            }
            TableFactor::NestedJoin { table_with_joins, alias: None } => self.add(frame, table_with_joins),
            other => {
                frame.opaque = true;
                let alias = match other {
                    TableFactor::Derived { alias, .. }
                    | TableFactor::NestedJoin { alias, .. }
                    | TableFactor::TableFunction { alias, .. }
                    | TableFactor::Function { alias, .. }
                    | TableFactor::UNNEST { alias, .. } => alias.as_ref(),
                    _ => None,
                };
                if let Some(a) = alias {
                    frame.names.insert(a.name.value.to_lowercase(), None);
                }
            }
        }
    }

    fn column(&self, table: &str, col: &str) -> Option<(String, usize)> {
        let j = self.schema.get(table)?.cols.iter().position(|c| c.name.eq_ignore_ascii_case(col))?;
        Some((table.to_string(), j))
    }

    /// The table and column a reference names, from its lower-cased parts.
    fn resolve(&self, parts: &[String]) -> Option<(String, usize)> {
        let col = parts.last()?;
        if parts.len() >= 2 {
            let qualifier = &parts[parts.len() - 2];
            let binding = self.frames.iter().rev().find_map(|f| f.names.get(qualifier))?;
            return self.column(binding.as_ref()?, col);
        }
        for frame in self.frames.iter().rev() {
            let mut found: Vec<(String, usize)> =
                frame.names.values().flatten().filter_map(|t| self.column(t, col)).collect();
            found.sort();
            found.dedup();
            match (found.len(), frame.opaque) {
                (1, false) => return found.pop(),
                (0, false) => continue,
                _ => return None,
            }
        }
        None
    }

    fn link(&mut self, column: &Expr, param: &Expr, selects: bool, array: bool) {
        let (Some((parts, cast)), Some(n)) = (column_ref(column), placeholder(param)) else { return };
        if let Some((table, col)) = self.resolve(&parts) {
            self.out.insert(Link { param: n, table, col, selects, cast, array });
        }
    }

    /// Two operands of a comparison, either way round. Two rows are compared field by field.
    fn compared(&mut self, a: &Expr, b: &Expr, selects: bool) {
        if let (Expr::Tuple(xs), Expr::Tuple(ys)) = (unnested(a), unnested(b)) {
            if xs.len() == ys.len() {
                for (x, y) in xs.iter().zip(ys) {
                    self.compared(x, y, selects);
                }
            }
            return;
        }
        self.link(a, b, selects, false);
        self.link(b, a, selects, false);
    }
}

fn unnested(mut e: &Expr) -> &Expr {
    while let Expr::Nested(inner) = e {
        e = inner;
    }
    e
}

/// A column reference's lower-cased parts, under any parentheses and casts, and whether a cast
/// was among them.
fn column_ref(e: &Expr) -> Option<(Vec<String>, bool)> {
    match e {
        Expr::Identifier(id) => Some((vec![id.value.to_lowercase()], false)),
        Expr::CompoundIdentifier(parts) => Some((parts.iter().map(|id| id.value.to_lowercase()).collect(), false)),
        Expr::Nested(x) => column_ref(x),
        Expr::Cast { expr, .. } => column_ref(expr).map(|(parts, _)| (parts, true)),
        _ => None,
    }
}

/// The `$N` of a parameter under any parentheses and casts.
fn placeholder(e: &Expr) -> Option<u32> {
    match e {
        Expr::Value(v) => match &v.value {
            Value::Placeholder(p) => p.strip_prefix('$')?.parse().ok(),
            _ => None,
        },
        Expr::Nested(x) | Expr::Cast { expr: x, .. } => placeholder(x),
        _ => None,
    }
}

impl Visitor for Links<'_> {
    type Break = ();

    fn pre_visit_statement(&mut self, st: &Statement) -> ControlFlow<()> {
        let mut frame = Frame::default();
        match st {
            Statement::Update(u) => {
                self.add(&mut frame, &u.table);
                if let Some(UpdateTableFromKind::BeforeSet(from) | UpdateTableFromKind::AfterSet(from)) = &u.from {
                    from.iter().for_each(|t| self.add(&mut frame, t));
                }
            }
            Statement::Delete(d) => {
                let (FromTable::WithFromKeyword(from) | FromTable::WithoutKeyword(from)) = &d.from;
                from.iter().for_each(|t| self.add(&mut frame, t));
                d.using.iter().flatten().for_each(|t| self.add(&mut frame, t));
            }
            _ => {}
        }
        self.frames.push(frame);
        ControlFlow::Continue(())
    }

    fn post_visit_statement(&mut self, _: &Statement) -> ControlFlow<()> {
        self.frames.pop();
        ControlFlow::Continue(())
    }

    fn pre_visit_query(&mut self, q: &Query) -> ControlFlow<()> {
        // A name a `WITH` binds is that binding wherever the query can see it, and inside the
        // `WITH` itself, where taking it for the binding only withholds a link.
        let names = q.with.iter().flat_map(|w| &w.cte_tables).map(|c| c.alias.name.value.to_lowercase()).collect();
        self.ctes.push(names);
        ControlFlow::Continue(())
    }

    fn post_visit_query(&mut self, _: &Query) -> ControlFlow<()> {
        self.ctes.pop();
        ControlFlow::Continue(())
    }

    fn pre_visit_select(&mut self, select: &Select) -> ControlFlow<()> {
        let mut frame = Frame::default();
        select.from.iter().for_each(|t| self.add(&mut frame, t));
        self.frames.push(frame);
        ControlFlow::Continue(())
    }

    fn post_visit_select(&mut self, _: &Select) -> ControlFlow<()> {
        self.frames.pop();
        ControlFlow::Continue(())
    }

    fn pre_visit_expr(&mut self, e: &Expr) -> ControlFlow<()> {
        use BinaryOperator as B;
        match e {
            Expr::BinaryOp { left, op: B::Eq | B::GtEq | B::LtEq, right } => self.compared(left, right, true),
            Expr::BinaryOp { left, op: B::NotEq | B::Lt | B::Gt, right } => self.compared(left, right, false),
            Expr::IsNotDistinctFrom(a, b) => self.compared(a, b, true),
            Expr::IsDistinctFrom(a, b) => self.compared(a, b, false),
            Expr::Between { expr, negated, low, high } => {
                self.compared(expr, low, !negated);
                self.compared(expr, high, !negated);
            }
            Expr::In { expr, target: InTarget::List(list), negated, .. } => {
                for item in list {
                    self.compared(expr, item, !negated);
                }
            }
            Expr::Like { negated, any: false, expr, pattern, .. }
            | Expr::ILike { negated, any: false, expr, pattern, .. } => self.link(expr, pattern, !negated, false),
            Expr::AnyOp { left, compare_op, right, .. } => {
                let selects = *compare_op == B::Eq;
                self.link(left, right, selects, true);
                self.link(right, left, selects, false);
            }
            Expr::AllOp { left, right, .. } => {
                self.link(left, right, false, true);
                self.link(right, left, false, false);
            }
            _ => {}
        }
        ControlFlow::Continue(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fuzz::schema::parse_schema;

    const DDL: &str = "CREATE TABLE a (id int, note text); \
                       CREATE TABLE orders (id int, tenant text, status text, created_at timestamp, tags text[]); \
                       CREATE TABLE users (uid int, tenant text);";

    /// The links of one statement, as `(param, table, column name, selects, cast, array)`.
    fn links(sql: &str) -> Vec<(u32, String, String, bool, bool, bool)> {
        let schema = parse_schema(DDL);
        row_links(sql, "", &schema)
            .into_iter()
            .map(|l| {
                let col = schema[&l.table].cols[l.col].name.clone();
                (l.param, l.table, col, l.selects, l.cast, l.array)
            })
            .collect()
    }

    fn link(n: u32, table: &str, col: &str) -> (u32, String, String, bool, bool, bool) {
        (n, table.to_string(), col.to_string(), true, false, false)
    }

    #[test]
    fn a_quoted_and_a_qualified_name_are_read() {
        // #301's review: the pattern over the text reads no quoted identifier.
        let want = vec![link(1, "orders", "tenant"), link(2, "orders", "status")];
        for sql in [
            "SELECT id FROM orders WHERE tenant = $1 AND status = $2",
            r#"SELECT "id" FROM "orders" WHERE "tenant" = $1 AND "status" = $2"#,
            r#"SELECT "o"."id" FROM "orders" AS "o" WHERE "o"."tenant" = $1 AND "o"."status" = $2"#,
            r#"SELECT id FROM public.orders WHERE public.orders.tenant = ($1) AND $2 = "Status""#,
        ] {
            assert_eq!(links(sql), want, "{sql}");
        }
    }

    #[test]
    fn a_qualifier_names_the_table_whatever_table_declares_the_name_first() {
        // `a` declares an `id` first, and the pattern over the text sent `o.id` there.
        assert_eq!(
            links("SELECT o.id FROM orders o JOIN a ON a.id = o.id WHERE o.id = $1 AND o.status = $2"),
            vec![link(1, "orders", "id"), link(2, "orders", "status")]
        );
        // A bare name two tables of the `FROM` have links nothing: Postgres rejects it, or it is
        // a `USING` column, and one of the two would be a guess.
        assert_eq!(links("SELECT 1 FROM orders JOIN a USING (id) WHERE id = $1 AND status = $2"), vec![link(2, "orders", "status")]);
        // One only one of them has is that one's.
        assert_eq!(links("SELECT 1 FROM orders, users WHERE uid = $1 AND status = $2"), vec![link(1, "users", "uid"), link(2, "orders", "status")]);
        // Two aliases of one table are one table.
        assert_eq!(
            links("SELECT 1 FROM orders x JOIN orders y ON x.id = y.id WHERE x.tenant = $1 AND y.status = $2"),
            vec![link(1, "orders", "tenant"), link(2, "orders", "status")]
        );
    }

    #[test]
    fn a_name_that_is_not_a_table_of_the_schema_links_nothing() {
        for sql in [
            // A derived table and a CTE, qualified and bare; a CTE that shadows a table's name.
            "SELECT 1 FROM (SELECT * FROM orders) q WHERE q.tenant = $1 AND q.status = $2",
            "SELECT 1 FROM (SELECT * FROM orders) q WHERE tenant = $1 AND status = $2",
            "WITH c AS (SELECT * FROM orders) SELECT 1 FROM c WHERE c.tenant = $1 AND status = $2",
            "WITH orders AS (SELECT 1 AS tenant, 2 AS status) SELECT 1 FROM orders WHERE tenant = $1 AND orders.status = $2",
            // An alias that renames the columns, a function, and a base table beside a derived one.
            "SELECT 1 FROM orders AS o (x, tenant) WHERE o.tenant = $1",
            "SELECT 1 FROM generate_series(1, 3) AS tenant WHERE tenant = $1",
            "SELECT 1 FROM orders, (SELECT 1 AS tenant) q WHERE tenant = $1",
            // A qualifier nothing in scope binds, and a column the table has not got.
            "SELECT 1 FROM orders WHERE nope.tenant = $1 AND orders.nope = $2",
            // Not a statement.
            "SELECT FROM WHERE tenant = $1",
        ] {
            assert_eq!(links(sql), vec![], "{sql}");
        }
    }

    #[test]
    fn a_subquery_resolves_in_its_own_from_first() {
        // The inner `tenant` is `users`', the outer one `orders`'; `o.status` is correlated.
        assert_eq!(
            links(
                "SELECT 1 FROM orders o WHERE tenant = $1 AND EXISTS \
                 (SELECT 1 FROM users WHERE tenant = $2 AND uid = $3 AND o.status = $4)"
            ),
            vec![link(1, "orders", "tenant"), link(2, "users", "tenant"), link(3, "users", "uid"), link(4, "orders", "status")]
        );
    }

    #[test]
    fn each_comparison_says_whether_it_selects_the_columns_own_value() {
        let got = links(
            "SELECT 1 FROM orders WHERE created_at >= $1 AND created_at < $2 AND id <> $3 AND tenant IN ($4, $5) \
             AND status NOT IN ($6) AND id BETWEEN $7 AND $8 AND tenant IS NOT DISTINCT FROM $9 AND status LIKE $10 \
             AND (id, tenant) = ($11, $12) AND $13 <= id AND status NOT LIKE $14",
        );
        let selecting: Vec<u32> = got.iter().filter(|l| l.3).map(|l| l.0).collect();
        let not: Vec<u32> = got.iter().filter(|l| !l.3).map(|l| l.0).collect();
        assert_eq!(selecting, [1, 4, 5, 7, 8, 9, 10, 11, 12, 13], "{got:?}");
        assert_eq!(not, [2, 3, 6, 14], "{got:?}");
    }

    #[test]
    fn a_cast_and_an_array_are_told() {
        assert_eq!(
            links("SELECT 1 FROM orders WHERE created_at::date = $1 AND CAST(id AS text) = $2::text AND status = ANY($3) AND $4 = ANY(tags)"),
            vec![
                (1, "orders".into(), "created_at".into(), true, true, false),
                (2, "orders".into(), "id".into(), true, true, false),
                (3, "orders".into(), "status".into(), true, false, true),
                (4, "orders".into(), "tags".into(), true, false, false),
            ]
        );
    }

    #[test]
    fn an_update_and_a_delete_link_their_conditions_and_no_assignment() {
        // `SET status = $1` stores the parameter: bound to the value the row already holds, the
        // statement would change nothing.
        assert_eq!(
            links("UPDATE orders SET status = $1 WHERE id = $2 AND tenant = $3"),
            vec![link(2, "orders", "id"), link(3, "orders", "tenant")]
        );
        assert_eq!(
            links("UPDATE orders o SET status = $1 FROM users u WHERE u.tenant = o.tenant AND u.uid = $2 AND o.id = $3"),
            vec![link(2, "users", "uid"), link(3, "orders", "id")]
        );
        assert_eq!(
            links("DELETE FROM orders USING users WHERE orders.tenant = users.tenant AND uid = $1 AND status = $2"),
            vec![link(1, "users", "uid"), link(2, "orders", "status")]
        );
    }
}
