// Unless explicitly stated otherwise all files in this repository are licensed under the
// Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/).
// Copyright 2026-Present Datadog, Inc.

//! Structural (AST-located) rewrites of a query.
//!
//! Distinct in kind from [`crate::patterns`], which matches query *text*: everything here parses the
//! statement with the same dialect the rest of the crate uses, finds the construct as an **AST node**,
//! and then edits only the byte range that node's span covers. Nothing is reconstructed — every byte we
//! do not deliberately replace is copied through untouched — so a rewrite cannot perturb formatting,
//! operator spelling, comments, or quoting anywhere else in the statement.
//!
//! A rewrite here is a no-op whenever the statement does not parse or the construct is absent. That
//! fallback is what makes the module safe to sit in the hot path: a pair we cannot rewrite is left
//! exactly as it arrived, which is the behaviour it had before.

use std::collections::BTreeMap;
use std::ops::ControlFlow;

use sqlparser::ast::{
    ArrayElemTypeDef, BinaryOperator, DataType, ExactNumberInfo, Expr, ObjectName, Select,
    SelectItem, SelectItemQualifiedWildcardKind, Spanned, Statement, Visit, VisitMut, Visitor,
    VisitorMut,
};
use sqlparser::dialect::PostgreSqlDialect;
use sqlparser::keywords::Keyword;
use sqlparser::parser::Parser;
use sqlparser::tokenizer::{Location, Span, Token, Tokenizer};

/// Byte offset of a 1-based (line, char-column) [`Location`].
///
/// sqlparser counts columns in `char`s, not bytes, so a statement containing any multi-byte character
/// before the construct would put a naive `column - 1` inside a character. Walking `char_indices` is
/// the conversion that cannot be off. Returns `None` for the `line: 0` empty span sqlparser reports
/// when it has no location, and for a location past the end of `sql`.
fn byte_of(sql: &str, l: Location) -> Option<usize> {
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

/// Spans of every `->` / `->>` application in a statement.
#[derive(Default)]
struct JsonOps(Vec<Span>);

impl Visitor for JsonOps {
    type Break = ();

    fn pre_visit_expr(&mut self, expr: &Expr) -> ControlFlow<()> {
        if let Expr::BinaryOp {
            op: BinaryOperator::Arrow | BinaryOperator::LongArrow,
            ..
        } = expr
        {
            self.0.push(expr.span());
        }
        ControlFlow::Continue(())
    }
}

/// Parenthesize every JSON accessor application: `a AND x ->> 'k' IS NULL` → `a AND (x ->> 'k') IS NULL`.
///
/// **DuckDB binds `->` and `->>` looser than `AND`, `OR` and `NOT`; Postgres binds them tighter than
/// every comparison.** So DuckDB reads an unparenthesized accessor that follows a conjunction as
/// taking that whole conjunction for its left operand:
///
/// ```text
///   WHERE id IN (SELECT ..) AND fields ->> 'b' IS NULL
///   Postgres:  id IN (SELECT ..) AND ((fields ->> 'b') IS NULL)
///   DuckDB:    ((id IN (SELECT ..) AND fields) ->> 'b') IS NULL
/// ```
///
/// Usually that is loud — the boolean will not coerce and the statement is a `Binder Error`, which
/// costs the pair its verdict. When the coercion happens to succeed it is silent and much worse: the
/// side is evaluated as a *different query*, and a pair whose two sides merely differ in how much of
/// the predicate they parenthesize — which is most of what a rewrite does — reports a counterexample
/// for a difference that exists only in DuckDB.
///
/// This is neither hypothetical nor a hygiene nit. Pairs with the shape on one side and not the
/// other have been observed as refutations, and among them pairs a prover independently calls
/// equivalent — the soundness-alarm cell of the cross-tab, reached from the disproving side. So it
/// is a false-refutation channel, and `test_pair` runs this pass on both sides before anything
/// reaches DuckDB.
///
/// The parenthesization is read off the **Postgres** parse, not guessed: the span of a
/// [`BinaryOperator::Arrow`] / [`BinaryOperator::LongArrow`] node is by construction the operand
/// extent Postgres gave it, so wrapping that span is a no-op for Postgres meaning and leaves DuckDB
/// no freedom to re-associate. Nested accessors (`a -> 'b' ->> 'c'`) nest their spans and so nest
/// their parentheses.
pub fn parenthesize_json_ops(sql: &str) -> String {
    let Ok(stmts) = Parser::parse_sql(&PostgreSqlDialect {}, sql) else {
        return sql.to_string();
    };
    let mut ops = JsonOps::default();
    for st in &stmts {
        let _ = st.visit(&mut ops);
    }
    if ops.0.is_empty() {
        return sql.to_string();
    }

    // Insertion counts per byte offset. Unlike `unqualify_stars` these spans legitimately nest, so
    // the edits are insertions at points rather than replacements of ranges.
    let mut opens: BTreeMap<usize, usize> = BTreeMap::new();
    let mut closes: BTreeMap<usize, usize> = BTreeMap::new();
    for sp in &ops.0 {
        let (Some(s), Some(e)) = (byte_of(sql, sp.start), byte_of(sql, sp.end)) else {
            return sql.to_string(); // a span we cannot place drops the whole rewrite
        };
        if s >= e {
            return sql.to_string();
        }
        *opens.entry(s).or_default() += 1;
        *closes.entry(e).or_default() += 1;
    }

    let mut out = String::with_capacity(sql.len() + 2 * ops.0.len());
    for (i, ch) in sql.char_indices() {
        // Closers first: a node that ends where another begins encloses neither, and a node nested
        // inside another can never end after it — so this order is the only one that can be right.
        for _ in 0..closes.get(&i).copied().unwrap_or(0) {
            out.push(')');
        }
        for _ in 0..opens.get(&i).copied().unwrap_or(0) {
            out.push('(');
        }
        out.push(ch);
    }
    for _ in 0..closes.get(&sql.len()).copied().unwrap_or(0) {
        out.push(')');
    }

    // The pass claims to add only the grouping Postgres already applies, so the rewritten statement
    // must have *the same Postgres parse* as the one that came in. That is checkable, so check it on
    // every row rather than trusting the spans: sqlparser 0.62 reports the span of `CAST(x AS t)`
    // starting at `x` instead of at the keyword, so the accessor span for
    // `CAST ( t.d AS jsonb ) -> $1` is `t.d AS jsonb ) -> $1` and wrapping it yields
    // `CAST ( (t.d AS jsonb ) -> $1)` — which cost four rows of that reference run their verdict
    // to a parser error. The `x::t` spelling of the same cast spans correctly, so this is not a
    // shape one can screen for by looking at the query; and a span defect elsewhere in the AST would
    // be just as quiet. Validating the output covers the whole class at the price of one reparse on
    // the rare statement that has a JSON accessor in it.
    match Parser::parse_sql(&PostgreSqlDialect {}, &out) {
        Ok(after) if denested(&after) == denested(&stmts) => out,
        _ => sql.to_string(),
    }
}

/// Drop every redundant `(..)` so two parses can be compared for equality.
///
/// sqlparser records parentheses that do not change the tree as [`Expr::Nested`] nodes, and adding
/// some is this pass's whole job — so a comparison has to look past them. It loses nothing: grouping
/// is carried by the *shape* of the tree, not by these nodes, which is why `(a AND b) OR c` and
/// `a AND (b OR c)` stay different after stripping. So equality of the stripped parses says exactly
/// what we want it to say — the rewrite moved parentheses around and changed nothing else.
fn denested(stmts: &[Statement]) -> Vec<Statement> {
    let mut out = stmts.to_vec();
    for st in &mut out {
        let _ = VisitMut::visit(st, &mut Denest);
    }
    out
}

struct Denest;

impl VisitorMut for Denest {
    type Break = ();

    // Post-order: the children are already stripped, so `((x))` collapses in one pass.
    fn post_visit_expr(&mut self, expr: &mut Expr) -> ControlFlow<()> {
        while let Expr::Nested(inner) = expr {
            let inner = (**inner).clone();
            *expr = inner;
        }
        ControlFlow::Continue(())
    }
}

/// Rewrite every bare `float` cast target to `DOUBLE`: `x::float` → `x::DOUBLE`,
/// `CAST(x AS float)` → `CAST(x AS DOUBLE)`, `x::float[]` → `x::DOUBLE[]`.
///
/// **Postgres's bare `float` is `double precision`; DuckDB's is the 4-byte `REAL`.** So DuckDB
/// evaluates a `::float` cast in single precision, and a pair whose two sides spell one Postgres type
/// two ways — `::float` on one side, `::double precision` on the other — computes different values
/// whenever the result is not exactly representable in single precision (`1/3` is `0.33333334`
/// against `0.3333333333333333`). That is a false refutation. Every other spelling already means the
/// same in both engines — `float(p)` (single up to 24 bits, double from 25), `float4`, `float8`,
/// `real`, `double precision` — so only the bare word is touched.
///
/// The edit is found by token and checked against the parse: the output must parse to exactly the
/// input's tree with each bare-`float` cast target made `DOUBLE`, and is discarded otherwise. That
/// rejects a `float` token that was not a cast target (an alias spelled `AS float`) as surely as a
/// cast target the token scan missed, so the scan can stay simple — and a statement the check rejects
/// keeps the single-precision reading it had before, which costs a verdict at worst.
pub fn double_precision_floats(sql: &str) -> String {
    let Ok(stmts) = Parser::parse_sql(&PostgreSqlDialect {}, sql) else {
        return sql.to_string();
    };
    let Ok(tokens) = Tokenizer::new(&PostgreSqlDialect {}, sql).tokenize_with_location() else {
        return sql.to_string();
    };
    let words: Vec<_> = tokens
        .iter()
        .filter(|t| !matches!(t.token, Token::Whitespace(_)))
        .collect();
    // An unquoted `FLOAT` right after `::` or `AS`, with no `(precision)` after it.
    let mut starts = Vec::new();
    for (i, t) in words.iter().enumerate() {
        let Token::Word(w) = &t.token else { continue };
        if w.keyword != Keyword::FLOAT || w.quote_style.is_some() {
            continue;
        }
        let cast_target = i > 0
            && match &words[i - 1].token {
                Token::DoubleColon => true,
                Token::Word(prev) => prev.keyword == Keyword::AS,
                _ => false,
            };
        let precision = matches!(words.get(i + 1).map(|n| &n.token), Some(Token::LParen));
        if cast_target && !precision {
            let Some(b) = byte_of(sql, t.span.start) else {
                return sql.to_string();
            };
            starts.push(b);
        }
    }
    if starts.is_empty() {
        return sql.to_string();
    }

    let mut out = String::with_capacity(sql.len() + starts.len());
    let mut cur = 0;
    for &b in &starts {
        let e = b + "float".len();
        if b < cur
            || !sql
                .get(b..e)
                .is_some_and(|w| w.eq_ignore_ascii_case("float"))
        {
            return sql.to_string();
        }
        out.push_str(&sql[cur..b]);
        out.push_str("DOUBLE");
        cur = e;
    }
    out.push_str(&sql[cur..]);

    match Parser::parse_sql(&PostgreSqlDialect {}, &out) {
        Ok(after) if after == retyped(&stmts) => out,
        _ => sql.to_string(),
    }
}

/// The parse with every bare-`float` cast target made `DOUBLE` — what [`double_precision_floats`]
/// claims its output parses to. `float(p)` is left alone, as the rewrite leaves it.
fn retyped(stmts: &[Statement]) -> Vec<Statement> {
    let mut out = stmts.to_vec();
    for st in &mut out {
        let _ = VisitMut::visit(st, &mut Retype);
    }
    out
}

struct Retype;

impl VisitorMut for Retype {
    type Break = ();

    fn post_visit_expr(&mut self, expr: &mut Expr) -> ControlFlow<()> {
        if let Expr::Cast { data_type, .. } = expr {
            double_the_float(data_type);
        }
        ControlFlow::Continue(())
    }
}

fn double_the_float(t: &mut DataType) {
    match t {
        DataType::Float(ExactNumberInfo::None) => *t = DataType::Double(ExactNumberInfo::None),
        DataType::Array(
            ArrayElemTypeDef::SquareBracket(inner, _)
            | ArrayElemTypeDef::AngleBracket(inner)
            | ArrayElemTypeDef::Parenthesis(inner)
            | ArrayElemTypeDef::Qualified(inner, _),
        ) => double_the_float(inner),
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::{
        double_precision_floats, parenthesize_json_ops, unqualify_stars, Parser, PostgreSqlDialect,
    };

    #[test]
    fn a_bare_float_cast_becomes_double() {
        for (sql, want) in [
            ("SELECT x::float FROM t", "SELECT x::DOUBLE FROM t"),
            (
                "SELECT CAST(x AS float) FROM t",
                "SELECT CAST(x AS DOUBLE) FROM t",
            ),
            ("SELECT x::float[] FROM t", "SELECT x::DOUBLE[] FROM t"),
            (
                "SELECT CAST(x AS float ARRAY) FROM t",
                "SELECT CAST(x AS DOUBLE ARRAY) FROM t",
            ),
            (
                "SELECT ROUND ( AVG ( c.p ),$1 ) :: FLOAT AS p FROM t c",
                "SELECT ROUND ( AVG ( c.p ),$1 ) :: DOUBLE AS p FROM t c",
            ),
            ("SELECT x::float", "SELECT x::DOUBLE"),
        ] {
            assert_eq!(double_precision_floats(sql), want, "{sql:?}");
        }
    }

    /// Every spelling that already means the same in both engines is returned byte-identical, and so
    /// is a `float` that is not a cast target: the parse check refuses the alias outright.
    #[test]
    fn only_the_bare_float_cast_moves() {
        for sql in [
            "SELECT x::float(24), x::float(53) FROM t",
            "SELECT x::float4, x::float8, x::real, x::double precision FROM t",
            "SELECT 1 AS float",
            "SELECT \"float\" FROM t",
            "SELECT 'x::float' FROM t -- y::float\n",
            "this is not sql at all",
        ] {
            assert_eq!(double_precision_floats(sql), sql, "rewrote {sql:?}");
        }
    }

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
    /// The clause that motivated the pass: unparenthesized in the source, parenthesized on the way
    /// to DuckDB, and the surrounding predicate untouched.
    #[test]
    fn an_accessor_after_a_conjunction_gets_its_own_parentheses() {
        assert_eq!(
            parenthesize_json_ops(
                "SELECT id FROM t WHERE id IN (SELECT a FROM u) AND fields ->> 'b' IS NULL"
            ),
            "SELECT id FROM t WHERE id IN (SELECT a FROM u) AND (fields ->> 'b') IS NULL",
        );
    }

    /// Already-parenthesized source is returned with the same grouping, not a second layer: the
    /// `Nested` node is not an accessor, so only the accessor inside it is wrapped.
    #[test]
    fn an_already_parenthesized_accessor_keeps_one_layer() {
        assert_eq!(
            parenthesize_json_ops("SELECT x FROM t WHERE a AND (fields ->> 'b') IS NULL"),
            "SELECT x FROM t WHERE a AND ((fields ->> 'b')) IS NULL",
        );
    }

    /// Chained accessors nest, left-associatively, exactly as Postgres parsed them.
    #[test]
    fn chained_accessors_nest() {
        assert_eq!(
            parenthesize_json_ops("SELECT fields -> 'a' ->> 'b' FROM t"),
            "SELECT ((fields -> 'a') ->> 'b') FROM t",
        );
    }

    /// Both spellings, in every expression position, and with a parameter as the key.
    #[test]
    fn every_position_and_both_spellings() {
        assert_eq!(
            parenthesize_json_ops(
                "SELECT a -> $1 AS k FROM t WHERE b ->> $2 = $3 GROUP BY c -> $4 ORDER BY d ->> $5"
            ),
            "SELECT (a -> $1) AS k FROM t WHERE (b ->> $2) = $3 GROUP BY (c -> $4) ORDER BY (d ->> $5)",
        );
    }

    /// Statements with no accessor — and text that only looks like one — come back byte-identical.
    #[test]
    fn nothing_without_an_accessor_moves() {
        for sql in [
            "SELECT a FROM t WHERE b = 1",
            "SELECT x FROM t WHERE note = 'a ->> b'",
            "SELECT a - 1 FROM t WHERE b > 2",
            "this is not sql at all",
        ] {
            assert_eq!(parenthesize_json_ops(sql), sql, "rewrote {sql:?}");
        }
    }

    /// Multi-byte text before the accessor does not shift the insertion points.
    #[test]
    fn a_multibyte_prefix_does_not_shift_the_parentheses() {
        assert_eq!(
            parenthesize_json_ops("SELECT /* ✂✂✂ */ fields ->> 'b' FROM t"),
            "SELECT /* ✂✂✂ */ (fields ->> 'b') FROM t",
        );
    }

    /// The pass preserves Postgres meaning by construction, so a Postgres-correct reading of the
    /// rewritten text must equal the original's: re-parsing the output and rewriting again adds
    /// only the parentheses the first pass already added.
    #[test]
    fn the_pass_is_idempotent_up_to_its_own_parentheses() {
        let once = parenthesize_json_ops(
            "SELECT id FROM t WHERE a AND fields ->> 'b' IS NULL AND c -> 'd' ->> 'e' = $1",
        );
        assert_eq!(
            parenthesize_json_ops(&once).replace(['(', ')'], ""),
            once.replace(['(', ')'], "")
        );
    }

    /// sqlparser 0.62 puts the span of `CAST(x AS t)` at `x`, not at the keyword, so the accessor
    /// span here is `t.d AS jsonb ) -> $2` and the naive rewrite is `CAST ( (t.d AS jsonb ) -> $2)`.
    /// The parse-equality guard has to catch that and hand the input back untouched — the row keeps
    /// whatever verdict it had instead of trading it for a parser error, which is what four rows of
    /// the reference run did before the guard existed.
    #[test]
    fn a_cast_keyword_span_is_declined() {
        let sql = "SELECT 1 FROM t WHERE a = $1 OR CAST ( t.d AS jsonb ) -> $2 = $3";
        assert_eq!(parenthesize_json_ops(sql), sql);
    }

    /// The `::` spelling of that same cast spans correctly, so it is still parenthesized: the guard
    /// declines the row it must and no more.
    #[test]
    fn a_colon_cast_is_still_wrapped() {
        assert_eq!(
            parenthesize_json_ops("SELECT 1 FROM t WHERE a = $1 OR t.d::jsonb -> $2 = $3"),
            "SELECT 1 FROM t WHERE a = $1 OR (t.d::jsonb -> $2) = $3"
        );
    }

    /// The guard's invariant, across every accessor shape above: whatever this pass returns parses.
    /// A pass whose only two outcomes are "parenthesized" and "unchanged" has no third behaviour
    /// left to audit downstream.
    #[test]
    fn the_output_always_parses() {
        for sql in [
            "SELECT 1 FROM t WHERE a = $1 AND t.d ->> 'k' IS NULL",
            "SELECT 1 FROM t WHERE a = $1 OR CAST ( t.d AS jsonb ) -> $2 = $3",
            "SELECT 1 FROM t WHERE a = $1 OR t.d::jsonb -> $2 = $3",
            "SELECT t.d -> 'a' ->> 'b' AS x FROM t WHERE NOT t.d ->> 'k' = 'v'",
            "UPDATE t SET d = $1 WHERE d ->> 'k' = $2 AND a = $3",
        ] {
            let out = parenthesize_json_ops(sql);
            assert!(
                Parser::parse_sql(&PostgreSqlDialect {}, &out).is_ok(),
                "rewrite does not parse:\n  in:  {sql}\n  out: {out}"
            );
        }
    }
}
