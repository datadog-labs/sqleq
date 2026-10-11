// Unless explicitly stated otherwise all files in this repository are licensed under the
// Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/).
// Copyright 2026-Present Datadog, Inc.

//! The tail's identifiers as Postgres resolves them, so that the two sides' tails can be compared.
//!
//! The checker requires the two sides' tails (alias, conflict clause, `RETURNING`) to be the same.
//! sqlparser's rendering keeps each identifier as written, so `excluded."id"` and `EXCLUDED.id`
//! would differ, although Postgres reads both as the column `id` of `excluded`. [`resolve`]
//! rewrites a copy of the tail so that two spellings of one name render alike:
//!
//! - An unquoted identifier is lower-cased, in ASCII only, as Postgres folds every unquoted
//!   identifier and keyword. That never changes what it means.
//! - Where the grammar reads a name, an unquoted identifier is also double-quoted, so that it meets
//!   the quoted spelling of the same name.
//!   - Some positions take any word as a name (Postgres's `ColLabel`): a part after a dot, an output
//!     alias. There every unquoted identifier is quoted.
//!   - The others (`ColId`) take any word but a reserved or `type_func_name` keyword (`KEYWORDS`):
//!     a column reference, the first part of a qualified name, a conflict column, an assignment
//!     target, the table alias. There such a keyword is left unquoted, because it is not that name:
//!     a bare `user` is the current user, not the column `"user"`; `DEFAULT` is not the column
//!     `"default"`; `AS user` does not parse.
//! - A function name is only lower-cased: `"coalesce"(a, b)` names no function at all.
//!
//! A quoted identifier is never changed, and nothing is changed outside these positions: literals,
//! types, collations, named arguments, and the names inside a subquery's `FROM`. Leaving a spelling
//! as written can only keep two equal tails apart, so the pair is refused, never proved.

use std::ops::ControlFlow;

use sqlparser::ast::{
    visit_expressions_mut, AssignmentTarget, ConflictTarget, Expr, Ident, ObjectName, ObjectNamePart,
    OnConflictAction, OnInsert, SelectItem, SelectItemQualifiedWildcardKind, TableAliasWithoutColumns,
};

/// The keywords Postgres's `ColId` does not take as a name: its reserved and `type_func_name`
/// keywords, `pg_get_keywords()` categories R and T, as of Postgres 17. The `col_name` keywords
/// (`timestamp`, `time`, `values`, …) are names there, which Postgres 17 bears out in every position
/// this module quotes. Treating a word as a keyword when it is not can only keep two spellings
/// apart. Sorted, for the binary search.
const KEYWORDS: &[&str] = &[
    "all", "analyse", "analyze", "and", "any", "array", "as", "asc", "asymmetric", "authorization",
    "binary", "both", "case", "cast", "check", "collate", "collation", "column", "concurrently",
    "constraint", "create", "cross", "current_catalog", "current_date", "current_role",
    "current_schema", "current_time", "current_timestamp", "current_user", "default", "deferrable",
    "desc", "distinct", "do", "else", "end", "except", "false", "fetch", "for", "foreign", "freeze",
    "from", "full", "grant", "group", "having", "ilike", "in", "initially", "inner", "intersect",
    "into", "is", "isnull", "join", "lateral", "leading", "left", "like", "limit", "localtime",
    "localtimestamp", "natural", "not", "notnull", "null", "offset", "on", "only", "or", "order",
    "outer", "overlaps", "placing", "primary", "references", "returning", "right", "select",
    "session_user", "similar", "some", "symmetric", "system_user", "table", "tablesample", "then",
    "to", "trailing", "true", "union", "unique", "user", "using", "variadic", "verbose", "when",
    "where", "window", "with",
];

fn keyword(word: &str) -> bool {
    KEYWORDS.binary_search(&word).is_ok()
}

/// Fold an unquoted identifier as Postgres does.
fn lower(id: &mut Ident) {
    if id.quote_style.is_none() {
        id.value.make_ascii_lowercase();
    }
}

/// A position where any word is a name (`ColLabel`).
fn label(id: &mut Ident) {
    lower(id);
    if id.quote_style.is_none() {
        id.quote_style = Some('"');
    }
}

/// A position where any word but a keyword is a name (`ColId`).
fn col_id(id: &mut Ident) {
    lower(id);
    if id.quote_style.is_none() && !keyword(&id.value) {
        id.quote_style = Some('"');
    }
}

/// A dotted name: the first part is a `ColId`, every later part a `ColLabel`.
fn path<'a>(parts: impl Iterator<Item = &'a mut Ident>) {
    for (i, id) in parts.enumerate() {
        if i == 0 {
            col_id(id)
        } else {
            label(id)
        }
    }
}

fn idents(name: &mut ObjectName) -> impl Iterator<Item = &mut Ident> {
    name.0.iter_mut().filter_map(|p| match p {
        ObjectNamePart::Identifier(id) => Some(id),
        ObjectNamePart::Function(_) => None,
    })
}

/// Every expression: column references, and function names.
fn exprs<V: sqlparser::ast::VisitMut>(node: &mut V) {
    let _ = visit_expressions_mut(node, |e| {
        match e {
            Expr::Identifier(id) => col_id(id),
            Expr::CompoundIdentifier(ids) => path(ids.iter_mut()),
            Expr::Function(f) => idents(&mut f.name).for_each(lower),
            _ => {}
        }
        ControlFlow::<()>::Continue(())
    });
}

/// Rewrite the tail's parts in place, as the module docs describe.
pub fn resolve(
    alias: &mut Option<TableAliasWithoutColumns>,
    on: &mut Option<OnInsert>,
    returning: &mut Option<Vec<SelectItem>>,
) {
    if let Some(a) = alias {
        col_id(&mut a.alias);
    }
    if let Some(OnInsert::OnConflict(c)) = on {
        match &mut c.conflict_target {
            Some(ConflictTarget::Columns(cols)) => cols.iter_mut().for_each(col_id),
            // Postgres takes one name here; a qualified one does not parse, and is left as written.
            Some(ConflictTarget::OnConstraint(name)) if name.0.len() == 1 => path(idents(name)),
            _ => {}
        }
        if let OnConflictAction::DoUpdate(u) = &mut c.action {
            for a in &mut u.assignments {
                match &mut a.target {
                    AssignmentTarget::ColumnName(n) => path(idents(n)),
                    AssignmentTarget::Tuple(ns) => ns.iter_mut().for_each(|n| path(idents(n))),
                }
            }
        }
    }
    if let Some(on) = on {
        exprs(on);
    }
    if let Some(items) = returning {
        for item in items.iter_mut() {
            match item {
                SelectItem::ExprWithAlias { alias, .. } => label(alias),
                SelectItem::QualifiedWildcard(SelectItemQualifiedWildcardKind::ObjectName(n), _) => {
                    path(idents(n))
                }
                _ => {}
            }
        }
        exprs(items);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::recognize::{recognize, Tail};
    use sqlparser::parser::Parser;

    fn key(sql: &str) -> Tail {
        let st = Parser::parse_sql(&sqleq_frontend::internals::DIALECT, sql).unwrap().remove(0);
        recognize(&st).unwrap_or_else(|e| panic!("{sql}: {e:?}")).key
    }

    fn same(tail_a: &str, tail_b: &str) -> bool {
        let ins = "INSERT INTO t (id, name) VALUES ($1, $2)";
        key(&format!("{ins} {tail_a}")) == key(&format!("{ins} {tail_b}"))
    }

    fn same_alias(alias_a: &str, alias_b: &str, tail: &str) -> bool {
        let ins = |a: &str| format!("INSERT INTO t {a} (id, name) VALUES ($1, $2) {tail}");
        key(&ins(alias_a)) == key(&ins(alias_b))
    }

    #[test]
    fn the_keywords_are_sorted_and_lower_case() {
        assert!(KEYWORDS.windows(2).all(|w| w[0] < w[1]));
        assert!(KEYWORDS.iter().all(|k| k.bytes().all(|b| b.is_ascii_lowercase() || b == b'_')));
    }

    #[test]
    fn spellings_postgres_resolves_alike_are_one_tail() {
        for (a, b) in [
            (
                "ON CONFLICT (id) DO UPDATE SET name = excluded.\"name\"",
                "ON CONFLICT (id) DO UPDATE SET name = excluded.name",
            ),
            (
                "ON CONFLICT (id) DO UPDATE SET name = EXCLUDED.name",
                "ON CONFLICT (id) DO UPDATE SET name = excluded.name",
            ),
            ("ON CONFLICT (\"id\") DO NOTHING", "ON CONFLICT (id) DO NOTHING"),
            (
                "ON CONFLICT (id) DO UPDATE SET \"name\" = \"excluded\".\"name\"",
                "ON CONFLICT (id) DO UPDATE SET name = excluded.name",
            ),
            (
                "ON CONFLICT (id) DO UPDATE SET name = excluded.name WHERE T.Name IS DISTINCT FROM EXCLUDED.NAME",
                "ON CONFLICT (id) DO UPDATE SET name = excluded.name WHERE t.name IS DISTINCT FROM excluded.name",
            ),
            ("ON CONFLICT ON CONSTRAINT \"t_pkey\" DO NOTHING", "ON CONFLICT ON CONSTRAINT T_PKEY DO NOTHING"),
            ("RETURNING id AS ID", "RETURNING \"id\" AS \"id\""),
            ("RETURNING LOWER(name)", "RETURNING lower(name)"),
            ("RETURNING Id, Name", "RETURNING id, name"),
            // A part after a dot is a name even when it is a keyword: `t.user` is the column.
            ("RETURNING t.user", "RETURNING t.\"user\""),
            ("RETURNING T.*", "RETURNING t.*"),
            // Only ASCII folds, so an unquoted `Éx` is the quoted `"Éx"`.
            ("RETURNING Éx", "RETURNING \"Éx\""),
            // A `col_name` keyword is a name in a column position.
            ("RETURNING id, timestamp", "RETURNING id, \"timestamp\""),
            (
                "ON CONFLICT (time) DO UPDATE SET values = excluded.values",
                "ON CONFLICT (\"time\") DO UPDATE SET \"values\" = excluded.\"values\"",
            ),
        ] {
            assert!(same(a, b), "{a}  vs  {b}");
        }
        assert!(same_alias("AS X", "AS \"x\"", "RETURNING X.id"));
        assert!(same_alias("AS x", "AS X", "RETURNING x.id"));
    }

    #[test]
    fn spellings_postgres_resolves_differently_stay_apart() {
        for (a, b) in [
            ("ON CONFLICT (id) DO UPDATE SET name = excluded.\"Name\"", "ON CONFLICT (id) DO UPDATE SET name = excluded.name"),
            ("RETURNING id AS 'id'", "RETURNING id AS \"id\""),
            // A keyword in a `ColId` position is not the quoted name.
            ("ON CONFLICT (id) DO UPDATE SET name = DEFAULT", "ON CONFLICT (id) DO UPDATE SET name = \"default\""),
            ("RETURNING user", "RETURNING \"user\""),
            ("RETURNING left", "RETURNING \"left\""),
            // A quoted function name is looked up as written, and can name no function at all.
            ("RETURNING \"lower\"(name)", "RETURNING lower(name)"),
            ("RETURNING \"coalesce\"(name, 'x')", "RETURNING coalesce(name, 'x')"),
            // Postgres folds ASCII only.
            ("RETURNING Éx", "RETURNING \"éx\""),
            // Qualifying a column changes nothing Postgres reads here, but the check does not know.
            ("RETURNING t.id", "RETURNING id"),
            ("RETURNING s.t.id", ""),
            // Literals are not identifiers.
            ("RETURNING 'Id'", "RETURNING 'id'"),
        ] {
            assert!(!same(a, b), "{a}  vs  {b}");
        }
        assert!(!same_alias("AS \"X\"", "AS x", "RETURNING id"));
    }

    #[test]
    fn the_original_spelling_is_kept_for_the_re_render() {
        let st = Parser::parse_sql(
            &sqleq_frontend::internals::DIALECT,
            "INSERT INTO t AS X (id) VALUES ($1) ON CONFLICT (id) DO UPDATE SET id = EXCLUDED.id RETURNING X.id",
        )
        .unwrap()
        .remove(0);
        let p = recognize(&st).unwrap();
        assert_eq!(p.tail.returning.as_deref(), Some("X.id"));
        assert_eq!(p.key.returning.as_deref(), Some("\"x\".\"id\""));
        assert_eq!(p.key.alias.as_deref(), Some("AS \"x\""));
    }
}
