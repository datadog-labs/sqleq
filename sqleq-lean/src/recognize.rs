// Unless explicitly stated otherwise all files in this repository are licensed under the
// Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/).
// Copyright 2026-Present Datadog, Inc.

//! Read one `INSERT` into the parts the Lean checker models, or say why not.
//!
//! The fragment is `INSERT INTO t [AS a] (c₁, …, cₖ) <source> [ON CONFLICT …] [RETURNING …]` where
//! the source is either `VALUES` rows of parameters and `NULL`s, or
//! `SELECT * FROM unnest($1::T₁[], …, $k::Tₖ[])`. Everything after the source is kept as rendered
//! text, the *tail*: the checker never interprets it, it only requires the two sides' tails to be
//! identical and parameter-free.
//!
//! **Exhaustiveness is checked, not assumed.** An `INSERT` field this module fails to read would be
//! a clause silently deleted from the Lean statement. So after extracting the parts, [`recognize`]
//! renders a statement from those parts alone, reparses it, and requires sqlparser's canonical
//! rendering of it to equal that of the original. A clause the extraction missed (a `WHERE` on the
//! unnest, an `ORDER BY` on the `VALUES`, a hint, a `WITH`) makes the two differ, and the statement
//! is refused.

use sqlparser::ast::{
    CastKind, DataType, Expr, Insert, ObjectName, Query, SelectItem, SetExpr, Statement, TableFactor,
    TableObject, Value,
};
use sqlparser::parser::Parser;

use crate::schema::{fold, last_name};

/// Why a statement or pair gets no proof attempt.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Refusal {
    /// Outside the fragment the checker models. Not a claim about the pair.
    Unsupported(String),
    /// SQL Postgres itself rejects, so there is no equivalence question to ask.
    InvalidSql(String),
}

impl Refusal {
    pub fn verdict(&self) -> &'static str {
        match self {
            Refusal::Unsupported(_) => "unsupported",
            Refusal::InvalidSql(_) => "invalid-sql",
        }
    }
    pub fn reason(&self) -> &str {
        match self {
            Refusal::Unsupported(r) | Refusal::InvalidSql(r) => r,
        }
    }
}

fn unsupported(r: impl Into<String>) -> Refusal {
    Refusal::Unsupported(r.into())
}

/// A `VALUES` cell: `$n`, optionally cast (by `CAST` or `::`) to a type, or `NULL`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Cell {
    Param(u32, Option<DataType>),
    Null,
}

/// An `unnest` argument `$param::elem[]`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UnnestArg {
    pub param: u32,
    pub elem: DataType,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Source {
    Values(Vec<Vec<Cell>>),
    Unnest {
        /// How the table function was named, `unnest` / `UNNEST`, for the re-render.
        func: String,
        /// sqlparser's dedicated `UNNEST` table factor (as opposed to a generic table function).
        dedicated: bool,
        args: Vec<UnnestArg>,
        /// `AS u(a, b)`: renames only, so `SELECT *` still yields every column in order.
        alias: Option<String>,
    },
}

/// Everything after the source, rendered. Compared as a whole across the two sides.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Tail {
    pub alias: Option<String>,
    pub on: Option<String>,
    pub returning: Option<String>,
}

impl Tail {
    /// One string for the whole tail, with a separator no rendering contains unquoted.
    pub fn text(&self) -> String {
        format!(
            "{}\u{1f}{}\u{1f}{}",
            self.alias.as_deref().unwrap_or(""),
            self.on.as_deref().unwrap_or(""),
            self.returning.as_deref().unwrap_or("")
        )
    }
}

#[derive(Clone, Debug)]
pub struct Parts {
    pub target: String,
    target_obj: ObjectName,
    /// Target columns, folded.
    pub cols: Vec<String>,
    col_text: Vec<String>,
    pub src: Source,
    /// Each `VALUES` cell or `unnest` argument as the original spells it, for the re-render.
    /// Spelling only: `(($1))` and `$1` are one cell, and the extraction already read which.
    src_text: Vec<Vec<String>>,
    pub tail: Tail,
    /// The conflict clause itself, for the witness. The tail already carries it as text.
    pub on: Option<sqlparser::ast::OnInsert>,
}

fn placeholder(e: &Expr) -> Option<u32> {
    match strip(e) {
        Expr::Value(v) => match &v.value {
            Value::Placeholder(s) => s.strip_prefix('$')?.parse().ok(),
            _ => None,
        },
        _ => None,
    }
}

fn strip(e: &Expr) -> &Expr {
    match e {
        Expr::Nested(inner) => strip(inner),
        other => other,
    }
}

/// `CAST(x AS t)` and `x::t` are one cast. `TRY_CAST` / `SAFE_CAST` turn a failure into NULL,
/// which is not a cast Postgres has.
fn postgres_cast(kind: &CastKind) -> bool {
    matches!(kind, CastKind::Cast | CastKind::DoubleColon)
}

fn cell(e: &Expr) -> Result<Cell, Refusal> {
    if let Some(n) = placeholder(e) {
        return Ok(Cell::Param(n, None));
    }
    match strip(e) {
        Expr::Value(v) if matches!(v.value, Value::Null) => Ok(Cell::Null),
        Expr::Cast { kind, expr, data_type, format: None, .. } => {
            let (true, Some(n)) = (postgres_cast(kind), placeholder(expr)) else {
                return Err(unsupported("VALUES cell is a cast of something other than a parameter"));
            };
            Ok(Cell::Param(n, Some(data_type.clone())))
        }
        Expr::Identifier(id) if id.quote_style.is_none() && id.value.eq_ignore_ascii_case("default") => {
            Err(unsupported("DEFAULT in a VALUES row"))
        }
        _ => Err(unsupported("VALUES cell is not a parameter, a cast parameter or NULL")),
    }
}

fn unnest_arg(e: &Expr) -> Result<UnnestArg, Refusal> {
    let Expr::Cast { kind, expr, data_type, format: None, .. } = strip(e) else {
        return Err(unsupported("unnest argument is not a parameter cast to an array type"));
    };
    let (true, Some(n)) = (postgres_cast(kind), placeholder(expr)) else {
        return Err(unsupported("unnest argument is not a parameter cast to an array type"));
    };
    let DataType::Array(elem) = data_type else {
        return Err(unsupported("unnest argument is cast to a non-array type"));
    };
    use sqlparser::ast::ArrayElemTypeDef as E;
    let elem = match elem {
        E::SquareBracket(t, None) | E::AngleBracket(t) | E::Parenthesis(t) => (**t).clone(),
        E::SquareBracket(_, Some(_)) => return Err(unsupported("unnest argument has a sized array type")),
        E::None => return Err(unsupported("unnest argument has an array type with no element type")),
        // Postgres reads `T ARRAY` as `T[]`, but the frontend refuses that spelling, and so does
        // this, so that the two agree on which pairs they will look at.
        E::Qualified(..) => return Err(unsupported("unnest argument uses the `T ARRAY` spelling")),
    };
    if matches!(elem, DataType::Array(_)) {
        return Err(unsupported("unnest argument is a multi-dimensional array"));
    }
    Ok(UnnestArg { param: n, elem })
}

fn source(q: &Query) -> Result<Source, Refusal> {
    match q.body.as_ref() {
        SetExpr::Values(v) => {
            let rows = v
                .rows
                .iter()
                .map(|r| r.content.iter().map(cell).collect::<Result<Vec<_>, _>>())
                .collect::<Result<Vec<_>, _>>()?;
            Ok(Source::Values(rows))
        }
        SetExpr::Select(sel) => {
            let [SelectItem::Wildcard(_)] = sel.projection.as_slice() else {
                return Err(unsupported("INSERT ... SELECT that is not SELECT * FROM unnest(..)"));
            };
            let [twj] = sel.from.as_slice() else {
                return Err(unsupported("INSERT ... SELECT that is not SELECT * FROM unnest(..)"));
            };
            if !twj.joins.is_empty() {
                return Err(unsupported("unnest source has a join"));
            }
            match &twj.relation {
                TableFactor::UNNEST { alias, array_exprs, with_offset: false, with_offset_alias: None, with_ordinality: false } => {
                    Ok(Source::Unnest {
                        func: "UNNEST".into(),
                        dedicated: true,
                        args: array_exprs.iter().map(unnest_arg).collect::<Result<_, _>>()?,
                        alias: alias.as_ref().map(|a| a.to_string()),
                    })
                }
                TableFactor::Function { lateral: false, name, args, with_ordinality: false, alias }
                    if last_name(name).as_deref() == Some("unnest") && name.0.len() == 1 =>
                {
                    use sqlparser::ast::{FunctionArg, FunctionArgExpr};
                    let exprs = args
                        .iter()
                        .map(|a| match a {
                            FunctionArg::Unnamed(FunctionArgExpr::Expr(e)) => unnest_arg(e),
                            _ => Err(unsupported("unnest argument is named or a wildcard")),
                        })
                        .collect::<Result<_, _>>()?;
                    Ok(Source::Unnest {
                        func: name.to_string(),
                        dedicated: false,
                        args: exprs,
                        alias: alias.as_ref().map(|a| a.to_string()),
                    })
                }
                TableFactor::UNNEST { .. } | TableFactor::Function { .. } => {
                    Err(unsupported("unnest WITH ORDINALITY / WITH OFFSET / LATERAL"))
                }
                _ => Err(unsupported("INSERT ... SELECT that is not SELECT * FROM unnest(..)")),
            }
        }
        _ => Err(unsupported("INSERT source is neither VALUES nor SELECT")),
    }
}

impl Parts {
    /// The target table as the statement names it.
    pub fn target_sql(&self) -> String {
        self.target_obj.to_string()
    }

    /// The statement these parts describe, as SQL text.
    fn render(&self) -> String {
        let src = match &self.src {
            Source::Values(_) => {
                let rows: Vec<String> =
                    self.src_text.iter().map(|r| format!("({})", r.join(", "))).collect();
                format!("VALUES {}", rows.join(", "))
            }
            Source::Unnest { func, alias, .. } => {
                let args: Vec<String> = self.src_text.first().cloned().unwrap_or_default();
                // sqlparser's rendering of the alias carries its own `AS` when one was written.
                let alias = alias.as_ref().map(|a| format!(" {a}")).unwrap_or_default();
                format!("SELECT * FROM {func}({}){alias}", args.join(", "))
            }
        };
        let alias = self.tail.alias.as_ref().map(|a| format!(" {a}")).unwrap_or_default();
        let on = self.tail.on.as_ref().map(|o| format!(" {o}")).unwrap_or_default();
        let ret = self.tail.returning.as_ref().map(|r| format!(" RETURNING {r}")).unwrap_or_default();
        format!(
            "INSERT INTO {}{alias} ({}) {src}{on}{ret}",
            self.target_obj,
            self.col_text.join(", ")
        )
    }
}

/// Read an `INSERT` into its [`Parts`]. `original` is the whole statement, for the re-render check.
pub fn recognize(original: &Statement) -> Result<Parts, Refusal> {
    let Statement::Insert(i) = original else {
        return Err(unsupported("statement is not an INSERT"));
    };
    let parts = extract(i)?;
    // The exhaustiveness check described in the module docs.
    let rendered = parts.render();
    let reparsed = Parser::parse_sql(&sqleq_frontend::internals::DIALECT, &rendered)
        .map_err(|e| unsupported(format!("re-render did not parse ({e})")))?;
    let [again] = reparsed.as_slice() else {
        return Err(unsupported("re-render is not one statement"));
    };
    let (want, got) = (original.to_string(), again.to_string());
    if want != got {
        let at = want.chars().zip(got.chars()).take_while(|(x, y)| x == y).count();
        let near: String = want.chars().skip(at.saturating_sub(12)).take(40).collect();
        return Err(unsupported(format!(
            "INSERT has a clause the Lean fragment does not model, near `{near}`"
        )));
    }
    Ok(parts)
}

fn extract(i: &Insert) -> Result<Parts, Refusal> {
    // Everything here but the source, target, column list and tail is refused. The re-render
    // check would catch these too; naming them gives a better reason.
    if i.or.is_some() || i.ignore || i.replace_into || i.priority.is_some() {
        return Err(unsupported("INSERT OR / IGNORE / REPLACE / <priority>"));
    }
    if i.overwrite || i.partitioned.is_some() || !i.after_columns.is_empty() {
        return Err(unsupported("INSERT OVERWRITE / PARTITION"));
    }
    if !i.assignments.is_empty() || i.output.is_some() || i.insert_alias.is_some() {
        return Err(unsupported("INSERT ... SET / OUTPUT / row alias"));
    }
    if i.settings.is_some() || i.format_clause.is_some() || !i.optimizer_hints.is_empty() {
        return Err(unsupported("INSERT ... SETTINGS / FORMAT / optimizer hint"));
    }
    if i.multi_table_insert_type.is_some()
        || !i.multi_table_into_clauses.is_empty()
        || !i.multi_table_when_clauses.is_empty()
    {
        return Err(unsupported("multi-table INSERT"));
    }
    if matches!(i.on, Some(sqlparser::ast::OnInsert::DuplicateKeyUpdate(_))) {
        return Err(unsupported("ON DUPLICATE KEY UPDATE"));
    }
    let TableObject::TableName(name) = &i.table else {
        return Err(unsupported("INSERT target is not a plain table"));
    };
    let Some(target) = last_name(name) else {
        return Err(unsupported("INSERT target is not a plain name"));
    };
    let Some(q) = i.source.as_deref() else {
        return Err(unsupported("INSERT ... DEFAULT VALUES"));
    };
    if i.columns.is_empty() {
        return Err(unsupported("INSERT without a column list"));
    }
    let mut cols = Vec::new();
    let mut col_text = Vec::new();
    for c in &i.columns {
        let [part] = c.0.as_slice() else {
            return Err(unsupported("qualified INSERT column"));
        };
        let Some(id) = part.as_ident() else {
            return Err(unsupported("INSERT column is not an identifier"));
        };
        let f = fold(id);
        if cols.contains(&f) {
            return Err(Refusal::InvalidSql(format!("INSERT lists column {f} twice")));
        }
        cols.push(f);
        col_text.push(id.to_string());
    }
    let src = source(q)?;
    let src_text = source_text(q);
    let tail = Tail {
        alias: i
            .table_alias
            .as_ref()
            .map(|a| format!("{}{}", if a.explicit { "AS " } else { "" }, a.alias)),
        on: i.on.as_ref().map(|o| o.to_string().trim().to_string()),
        returning: i.returning.as_ref().map(|items| {
            items.iter().map(|x| x.to_string()).collect::<Vec<_>>().join(", ")
        }),
    };
    Ok(Parts { target, target_obj: name.clone(), cols, col_text, src, src_text, tail, on: i.on.clone() })
}

/// The original text of each `VALUES` cell (one inner list per row), or of each `unnest` argument
/// (one inner list). Only called once [`source`] has accepted the shape.
fn source_text(q: &Query) -> Vec<Vec<String>> {
    match q.body.as_ref() {
        SetExpr::Values(v) => {
            v.rows.iter().map(|r| r.content.iter().map(|e| e.to_string()).collect()).collect()
        }
        SetExpr::Select(sel) => match sel.from.first().map(|t| &t.relation) {
            Some(TableFactor::UNNEST { array_exprs, .. }) => {
                vec![array_exprs.iter().map(|e| e.to_string()).collect()]
            }
            Some(TableFactor::Function { args, .. }) => vec![args.iter().map(|a| a.to_string()).collect()],
            _ => Vec::new(),
        },
        _ => Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(sql: &str) -> Statement {
        let mut v = Parser::parse_sql(&sqleq_frontend::internals::DIALECT, sql).unwrap();
        v.remove(0)
    }

    fn rec(sql: &str) -> Result<Parts, Refusal> {
        recognize(&parse(sql))
    }

    #[test]
    fn a_values_insert() {
        let p = rec("INSERT INTO t (a, b) VALUES ($1, $2::text), ($3, NULL)").unwrap();
        assert_eq!(p.cols, ["a", "b"]);
        let Source::Values(rows) = p.src else { panic!() };
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[1][1], Cell::Null);
        assert!(matches!(rows[0][1], Cell::Param(2, Some(_))));
    }

    #[test]
    fn an_unnest_insert_either_spelling() {
        for sql in [
            "INSERT INTO t (a, b) SELECT * FROM unnest($1::int[], $2::text[])",
            "INSERT INTO t (a, b) SELECT * FROM UNNEST(CAST($1 AS INT[]), $2::TEXT[]) AS u(x, y)",
        ] {
            let p = rec(sql).unwrap_or_else(|e| panic!("{sql}: {e:?}"));
            let Source::Unnest { args, .. } = p.src else { panic!() };
            assert_eq!(args.iter().map(|a| a.param).collect::<Vec<_>>(), [1, 2]);
        }
    }

    #[test]
    fn parenthesised_cells_are_the_same_cells() {
        let p = rec("INSERT INTO t (a, b) VALUES (($1), (($2)::text)), ($3, (NULL))").unwrap();
        let Source::Values(rows) = p.src else { panic!() };
        assert_eq!(rows[0][0], Cell::Param(1, None));
        assert!(matches!(rows[0][1], Cell::Param(2, Some(_))));
        assert_eq!(rows[1][1], Cell::Null);
    }

    #[test]
    fn the_tail_is_kept_whole() {
        let p = rec("INSERT INTO t AS x (a) VALUES ($1) ON CONFLICT (a) DO UPDATE SET a = EXCLUDED.a RETURNING a, x.a").unwrap();
        assert!(p.tail.on.as_deref().unwrap().starts_with("ON CONFLICT"));
        assert_eq!(p.tail.returning.as_deref(), Some("a, x.a"));
        assert!(p.tail.alias.is_some());
    }

    #[test]
    fn clauses_outside_the_fragment_are_refused() {
        for sql in [
            "INSERT INTO t (a) SELECT * FROM unnest($1::int[]) WHERE true",
            "INSERT INTO t (a) SELECT * FROM unnest($1::int[]) ORDER BY 1",
            "INSERT INTO t (a) SELECT * FROM unnest($1::int[]) WITH ORDINALITY",
            "INSERT INTO t (a) SELECT DISTINCT * FROM unnest($1::int[])",
            "INSERT INTO t (a) SELECT * FROM unnest($1::int[]) LIMIT 1",
            "INSERT INTO t (a) VALUES ($1) LIMIT 1",
            "INSERT INTO t (a) WITH w AS (SELECT 1) SELECT * FROM unnest($1::int[])",
            "INSERT INTO t (a) VALUES (DEFAULT)",
            "INSERT INTO t (a) VALUES (1)",
            "INSERT INTO t (a) VALUES (now())",
            "INSERT INTO t (a) SELECT * FROM unnest($1)",
            "INSERT INTO t (a) SELECT * FROM unnest(CAST($1 AS INT ARRAY))",
            "INSERT INTO t VALUES ($1)",
            "INSERT INTO t DEFAULT VALUES",
        ] {
            assert!(matches!(rec(sql), Err(Refusal::Unsupported(_))), "{sql} was not refused");
        }
    }

    #[test]
    fn a_repeated_column_is_invalid_sql() {
        assert!(matches!(rec("INSERT INTO t (a, a) VALUES ($1, $2)"), Err(Refusal::InvalidSql(_))));
    }
}
