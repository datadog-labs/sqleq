// Unless explicitly stated otherwise all files in this repository are licensed under the
// Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/).
// Copyright 2026-Present Datadog, Inc.

//! `LIMIT`, `OFFSET` and `FETCH`: which of them can cut rows, and whether the rows they keep are
//! determined.
//!
//! A cut over rows whose order leaves ties keeps an arbitrary choice among the tied rows, so two
//! runs of the *same* query can return different bags. [`crate::pair::test_pair`] then trusts only a
//! difference in cardinality, which no tie-break can change -- unless the count is a bare `$N` and
//! nothing else, which is bound so that it cuts nothing. A literal `OFFSET 0`, `LIMIT ALL` or
//! `LIMIT NULL` cuts nothing either.
//!
//! Everything is read off the Postgres parse, so `LIMIT (1)`, `FETCH FIRST ROW ONLY` and
//! `LIMIT ($1)` are seen for what they are. A statement that does not parse is scanned for the
//! keywords instead and every cut in it is taken to be nondeterministic.

use std::ops::ControlFlow;

use sqlparser::ast::{Expr, LimitClause, Query, Statement, Value, Visit, Visitor};
use sqlparser::dialect::PostgreSqlDialect;
use sqlparser::keywords::Keyword;
use sqlparser::parser::Parser;
use sqlparser::tokenizer::Token;

use crate::lex::significant;

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
}

/// The cuts in `sql`, one per query level that has one, subqueries and CTEs included.
pub fn cuts(sql: &str) -> Vec<Cut> {
    let Ok(stmts) = Parser::parse_sql(&PostgreSqlDialect {}, sql) else {
        return unparsed(sql);
    };
    let mut finder = Finder { out: Vec::new() };
    for st in &stmts {
        let _ = Statement::visit(st, &mut finder);
    }
    finder.out
}

/// A statement the parser rejected: any `LIMIT`/`OFFSET`/`FETCH` keyword is one cut that cuts,
/// which leaves only the cardinality comparison -- sound whatever the clause turns out to say.
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
        }]
    } else {
        Vec::new()
    }
}

struct Finder {
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

impl Visitor for Finder {
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
            self.out.push(cut);
        }
        ControlFlow::Continue(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_spelling_of_a_cut_is_found() {
        for sql in [
            "SELECT id FROM t ORDER BY n LIMIT 1",
            "SELECT id FROM t ORDER BY n LIMIT (1)",
            "SELECT id FROM t ORDER BY n FETCH FIRST ROW ONLY",
            "SELECT id FROM t ORDER BY n FETCH NEXT 2 ROWS ONLY",
            "SELECT id FROM t ORDER BY n OFFSET 1",
            "SELECT id FROM t ORDER BY n LIMIT $1 + 1",
        ] {
            let c = cuts(sql);
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
            let c = cuts(sql);
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
            assert!(cuts(sql).is_empty(), "{sql}: {:?}", cuts(sql));
        }
        // Subqueries each carry their own.
        assert_eq!(
            cuts("SELECT id FROM t WHERE id IN (SELECT id FROM u ORDER BY id LIMIT 1) LIMIT 2").len(),
            2
        );
    }

    #[test]
    fn an_unparsed_statement_with_a_cut_cuts() {
        let c = cuts("SELECT id FROM t ORDER BY id LIMIT 1 ;;; nonsense (");
        assert_eq!(
            c,
            vec![Cut {
                params: vec![],
                fixed: true,
            }]
        );
    }
}
