// Unless explicitly stated otherwise all files in this repository are licensed under the
// Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/).
// Copyright 2026-Present Datadog, Inc.

//! Structural (AST-located) rewrites that DuckDB needs in order to run a Postgres statement at all.
//!
//! Distinct in kind from [`crate::patterns`], which matches query *text*: everything here parses the
//! statement with the same dialect the rest of the crate uses, finds the construct as an **AST node**
//! or on the token stream, and then edits only the bytes it covers. Nothing is reconstructed — every
//! byte we do not deliberately replace is copied through untouched.
//!
//! Neither rewrite changes what a statement means: [`unqualify_stars`] respells a wildcard DuckDB's
//! parser rejects, and [`strip_public`] drops a qualifier DuckDB has no schema for. A rewrite is a
//! no-op whenever the statement does not parse or the construct is absent. Nothing here makes DuckDB
//! compute what Postgres computes: the DuckDB engine evaluates a pair as DuckDB does.

use std::ops::ControlFlow;

use sqlparser::ast::{
    Expr, ObjectName, Select, SelectItem, SelectItemQualifiedWildcardKind, Spanned, Visit, Visitor,
};
use sqlparser::dialect::PostgreSqlDialect;
use sqlparser::parser::Parser;
use sqlparser::tokenizer::{Location, Span, Token};

use crate::lex::{significant, Tok};

/// Byte offset of a 1-based (line, char-column) [`Location`].
///
/// sqlparser counts columns in `char`s, not bytes, so a statement containing any multi-byte character
/// before the construct would put a naive `column - 1` inside a character. Walking `char_indices` is
/// the conversion that cannot be off. Returns `None` for the `line: 0` empty span sqlparser reports
/// when it has no location, and for a location past the end of `sql`.
pub(crate) fn byte_of(sql: &str, l: Location) -> Option<usize> {
    if l.line == 0 || l.column == 0 {
        return None;
    }
    let (mut line, mut col) = (1u64, 1u64);
    for (b, c) in sql.char_indices() {
        if line == l.line && col == l.column {
            return Some(b);
        }
        if c == '\n' {
            line += 1;
            col = 1;
        } else {
            col += 1;
        }
    }
    (line == l.line && col == l.column).then_some(sql.len())
}

/// A qualified wildcard whose qualifier names more than one part: the span of the whole qualifier, and
/// the span of its final part.
struct Star {
    qualifier: Span,
    last: Span,
}

/// Collects every multi-part qualified wildcard in a statement.
///
/// Both AST shapes are covered: [`SelectItem::QualifiedWildcard`] is the projection form (`s.t.*`), and
/// [`Expr::QualifiedWildcard`] is the same construct in an expression position. The visitor reaches
/// every [`Select`] and every [`Expr`] node in the statement — inside set operations, CTEs, derived
/// tables, and the sub-queries of INSERT/UPDATE/DELETE alike — so no hand-rolled traversal is needed.
#[derive(Default)]
struct Stars(Vec<Star>);

impl Stars {
    fn push(&mut self, name: &ObjectName) {
        if name.0.len() < 2 {
            return; // already single-part: `t.*` is what we want to end at
        }
        if let Some(last) = name.0.last() {
            self.0.push(Star {
                qualifier: name.span(),
                last: last.span(),
            });
        }
    }
}

impl Visitor for Stars {
    type Break = ();

    fn pre_visit_select(&mut self, select: &Select) -> ControlFlow<()> {
        for item in &select.projection {
            if let SelectItem::QualifiedWildcard(
                SelectItemQualifiedWildcardKind::ObjectName(n),
                _,
            ) = item
            {
                self.push(n);
            }
        }
        ControlFlow::Continue(())
    }

    fn pre_visit_expr(&mut self, expr: &Expr) -> ControlFlow<()> {
        if let Expr::QualifiedWildcard(n, _) = expr {
            self.push(n);
        }
        ControlFlow::Continue(())
    }
}

/// Rewrite every multi-part qualified wildcard down to its final part: `schema.table.*` → `table.*`.
///
/// DuckDB's parser accepts a one-part wildcard qualifier only; `schema.table.*` is a hard
/// `syntax error at or near "*"`, which costs the pair its verdict entirely. Postgres accepts it,
/// so real query pairs contain it.
///
/// **This preserves meaning, it does not approximate it.** A wildcard qualifier resolves against the
/// FROM clause's *aliases*, and the implicit alias of a `schema.table` entry is `table`. Postgres is
/// therefore already resolving `schema.table.*` through the alias `table`, and a legal FROM clause
/// cannot hold two entries with the same alias (`table name "t" specified more than once`) — so
/// `table.*` is not a *guess* at the same relation, it is the same reference spelled without the
/// redundant schema. The qualifier is dropped from the wildcard only; qualified *column* references
/// elsewhere are left alone, because DuckDB parses those at any arity.
///
/// Should that reasoning ever fail to hold, the failure mode is safe rather than silent: DuckDB
/// answers an unresolvable or ambiguous wildcard qualifier with a hard `Binder Error`, never with a
/// guess, so such a pair reports an error — which is exactly what it reported before the rewrite.
///
/// The last part's *original bytes* are what get spliced in, so its quoting and case survive verbatim.
pub fn unqualify_stars(sql: &str) -> String {
    let Ok(stmts) = Parser::parse_sql(&PostgreSqlDialect {}, sql) else {
        return sql.to_string();
    };
    let mut stars = Stars::default();
    for st in &stmts {
        let _ = st.visit(&mut stars);
    }

    // (start, end, replacement), in source order. A span we cannot convert to byte offsets drops the
    // whole rewrite rather than half of it: a partial splice is the one outcome worse than none.
    let mut edits: Vec<(usize, usize, &str)> = Vec::with_capacity(stars.0.len());
    for s in &stars.0 {
        let (Some(qs), Some(qe), Some(ls), Some(le)) = (
            byte_of(sql, s.qualifier.start),
            byte_of(sql, s.qualifier.end),
            byte_of(sql, s.last.start),
            byte_of(sql, s.last.end),
        ) else {
            return sql.to_string();
        };
        if qs > ls || le > qe {
            return sql.to_string(); // last part not inside its own qualifier: don't trust the spans
        }
        edits.push((qs, qe, &sql[ls..le]));
    }
    if edits.is_empty() {
        return sql.to_string();
    }
    edits.sort_by_key(|e| e.0);

    let mut out = String::with_capacity(sql.len());
    let mut cur = 0usize;
    for (start, end, keep) in edits {
        if start < cur {
            return sql.to_string(); // overlapping spans: same reasoning as above
        }
        out.push_str(&sql[cur..start]);
        out.push_str(keep);
        cur = end;
    }
    out.push_str(&sql[cur..]);
    out
}

/// Drop a leading `public.` from every qualified name: `public.t` → `t`, `public.t.c` → `t.c`.
///
/// Under Postgres's default `search_path` (`"$user", public`) a bare `t` *is* `public.t`, so a pair
/// may spell one table both ways. DuckDB has no `public` schema to resolve either to; creating one
/// beside `main` would make the two spellings two tables, and in a mutation pair the side that wrote
/// through one of them would leave the other untouched -- a difference Postgres does not have. With
/// the qualifier gone both spellings are the table `t`, which is what they meant.
///
/// Found on the token stream, so `'public.t'` in a literal stays, and only where `public` is the
/// first part of a dotted name (not `x.public.t`). Kept only if the statement still parses.
pub fn strip_public(sql: &str) -> String {
    let Some(toks) = significant(sql) else {
        return sql.to_string();
    };
    let is_public = |t: &Tok| match &t.token {
        Token::Word(w) => match w.quote_style {
            None => w.value.eq_ignore_ascii_case("public"),
            Some('"') => w.value == "public",
            Some(_) => false,
        },
        _ => false,
    };
    let mut cuts: Vec<(usize, usize)> = Vec::new();
    for i in 0..toks.len() {
        let after_dot = i > 0 && matches!(toks[i - 1].token, Token::Period);
        if !after_dot
            && is_public(&toks[i])
            && matches!(toks.get(i + 1).map(|t| &t.token), Some(Token::Period))
            && matches!(toks.get(i + 2).map(|t| &t.token), Some(Token::Word(_)))
        {
            cuts.push((toks[i].start, toks[i + 1].end));
        }
    }
    if cuts.is_empty() || Parser::parse_sql(&PostgreSqlDialect {}, sql).is_err() {
        return sql.to_string();
    }
    let mut out = String::with_capacity(sql.len());
    let mut cur = 0;
    for (b, e) in cuts {
        out.push_str(&sql[cur..b]);
        cur = e;
    }
    out.push_str(&sql[cur..]);
    match Parser::parse_sql(&PostgreSqlDialect {}, &out) {
        Ok(_) => out,
        Err(_) => sql.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::unqualify_stars;

    #[test]
    fn a_three_part_star_loses_only_its_schema() {
        assert_eq!(
            unqualify_stars("SELECT part_9.attributes.* FROM part_9.attributes WHERE part_9.attributes.team_id = $1"),
            "SELECT attributes.* FROM part_9.attributes WHERE part_9.attributes.team_id = $1",
        );
    }

    /// Everything the rewrite has no business touching is returned byte-identical — including the
    /// forms DuckDB already parses, and the qualified *column* references that look alike.
    #[test]
    fn nothing_else_moves() {
        for sql in [
            "SELECT t.* FROM t",
            "SELECT * FROM s.t",
            "SELECT s.t.a, s.t.b FROM s.t",
            "SELECT a.b.c * 2 AS n FROM a.b",
            "SELECT x FROM t WHERE note = 'a.b.*'  -- s.t.* in a comment\n",
            "this is not sql at all",
        ] {
            assert_eq!(unqualify_stars(sql), sql, "rewrote {sql:?}");
        }
    }

    /// Quoting and case of the surviving part come from the source, not from a re-render.
    #[test]
    fn the_surviving_part_keeps_its_original_bytes() {
        assert_eq!(
            unqualify_stars(r#"SELECT "Pt"."Attributes".* FROM "Pt"."Attributes""#),
            r#"SELECT "Attributes".* FROM "Pt"."Attributes""#,
        );
    }

    /// Every occurrence is rewritten, in every position a `Select` can appear.
    #[test]
    fn every_occurrence_in_every_position() {
        assert_eq!(
            unqualify_stars("SELECT s.a.* FROM s.a UNION ALL SELECT s.b.* FROM s.b"),
            "SELECT a.* FROM s.a UNION ALL SELECT b.* FROM s.b",
        );
        assert_eq!(
            unqualify_stars("WITH c AS (SELECT s.a.* FROM s.a) SELECT c.* FROM c"),
            "WITH c AS (SELECT a.* FROM s.a) SELECT c.* FROM c",
        );
        assert_eq!(
            unqualify_stars("SELECT d.* FROM (SELECT s.a.* FROM s.a) d"),
            "SELECT d.* FROM (SELECT a.* FROM s.a) d",
        );
    }

    /// A four-part qualifier collapses all the way to one part in a single pass — the whole qualifier
    /// span is replaced, so arity is not something the rewrite has to iterate over.
    #[test]
    fn a_deeper_qualifier_collapses_in_one_pass() {
        assert_eq!(
            unqualify_stars("SELECT d.s.t.* FROM d.s.t"),
            "SELECT t.* FROM d.s.t"
        );
    }

    /// Multi-byte text before the construct does not shift the splice: sqlparser reports *char*
    /// columns, and the conversion walks characters.
    #[test]
    fn a_multibyte_prefix_does_not_shift_the_splice() {
        assert_eq!(
            unqualify_stars("SELECT /* ✂✂✂ */ s.t.* FROM s.t"),
            "SELECT /* ✂✂✂ */ t.* FROM s.t",
        );
    }
}
