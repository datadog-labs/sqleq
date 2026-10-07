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
//!
//! The one exception is [`postgres_operators`], whose rewrite is what keeps a comparison sound rather
//! than merely runnable: where it cannot place its edit it says so, and the pair gets no verdict.

use std::collections::BTreeMap;
use std::ops::ControlFlow;

use sqlparser::ast::{
    ArrayElemTypeDef, BinaryOperator, DataType, ExactNumberInfo, Expr, FunctionArg,
    FunctionArgExpr, FunctionArguments, Ident, ObjectName, ObjectNamePart, Select, SelectItem,
    SelectItemQualifiedWildcardKind, Spanned, Statement, UnaryOperator, Value, Visit, VisitMut,
    Visitor, VisitorMut,
};
use sqlparser::dialect::PostgreSqlDialect;
use sqlparser::keywords::Keyword;
use sqlparser::parser::Parser;
use sqlparser::tokenizer::{Location, Span, Token};

use crate::lex::{is_literal_token, lex, significant, Tok};
use crate::schema::BARE_NUMERIC;

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
    respell_cast_targets(sql, &[Keyword::FLOAT], "DOUBLE", double_the_float)
}

/// Rewrite every bare `numeric` / `decimal` cast target to a wide DECIMAL: `x::numeric` →
/// `x::DECIMAL(38,18)`, and likewise under `CAST(.. AS ..)` and with `[]`.
///
/// **Postgres's `numeric` with no typmod keeps every digit; DuckDB reads a bare `DECIMAL` as
/// `DECIMAL(18,3)`,** so `CAST(a * 0.0001 AS numeric) > 0` is false at `a = 2` in DuckDB and true in
/// Postgres. [`BARE_NUMERIC`] is the width the generated columns use for the same type, and why it is
/// wide enough is documented there. `numeric(p, s)` already means the same in both engines and is
/// left alone. Found and checked exactly as [`double_precision_floats`] is.
pub fn wide_numerics(sql: &str) -> String {
    let wide = format!("DECIMAL({},{})", BARE_NUMERIC.0, BARE_NUMERIC.1);
    respell_cast_targets(
        sql,
        &[Keyword::NUMERIC, Keyword::DECIMAL, Keyword::DEC],
        &wide,
        widen_the_numeric,
    )
}

/// Replace every unquoted cast-target word in `words` -- one right after `::` or `AS`, with no
/// `(precision)` after it -- by `replacement`, and keep the result only if it parses to exactly the
/// input's tree with `retype` applied to every cast's type.
fn respell_cast_targets(
    sql: &str,
    words: &[Keyword],
    replacement: &str,
    retype: fn(&mut DataType),
) -> String {
    let Ok(stmts) = Parser::parse_sql(&PostgreSqlDialect {}, sql) else {
        return sql.to_string();
    };
    let Some(toks) = significant(sql) else {
        return sql.to_string();
    };
    let mut spans = Vec::new();
    for (i, t) in toks.iter().enumerate() {
        let Token::Word(w) = &t.token else { continue };
        if !words.contains(&w.keyword) || w.quote_style.is_some() {
            continue;
        }
        let cast_target = i > 0
            && match &toks[i - 1].token {
                Token::DoubleColon => true,
                Token::Word(prev) => prev.keyword == Keyword::AS,
                _ => false,
            };
        let precision = matches!(toks.get(i + 1).map(|n| &n.token), Some(Token::LParen));
        if cast_target && !precision {
            spans.push((t.start, t.end));
        }
    }
    if spans.is_empty() {
        return sql.to_string();
    }

    let mut out = String::with_capacity(sql.len() + spans.len() * replacement.len());
    let mut cur = 0;
    for &(b, e) in &spans {
        out.push_str(&sql[cur..b]);
        out.push_str(replacement);
        cur = e;
    }
    out.push_str(&sql[cur..]);

    match Parser::parse_sql(&PostgreSqlDialect {}, &out) {
        Ok(after) if after == retyped(&stmts, retype) => out,
        _ => sql.to_string(),
    }
}

/// The parse with `retype` applied to every cast's type — what [`respell_cast_targets`] claims its
/// output parses to.
fn retyped(stmts: &[Statement], retype: fn(&mut DataType)) -> Vec<Statement> {
    let mut out = stmts.to_vec();
    for st in &mut out {
        let _ = VisitMut::visit(st, &mut Retype(retype));
    }
    out
}

struct Retype(fn(&mut DataType));

impl VisitorMut for Retype {
    type Break = ();

    fn post_visit_expr(&mut self, expr: &mut Expr) -> ControlFlow<()> {
        if let Expr::Cast { data_type, .. } = expr {
            (self.0)(data_type);
        }
        ControlFlow::Continue(())
    }
}

/// `float` → `DOUBLE`, through any array wrapping. `float(p)` is left alone, as the rewrite leaves it.
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

/// A bare `numeric`/`decimal`/`dec` → `DECIMAL(38,18)`, through any array wrapping.
fn widen_the_numeric(t: &mut DataType) {
    match t {
        DataType::Numeric(ExactNumberInfo::None)
        | DataType::Decimal(ExactNumberInfo::None)
        | DataType::Dec(ExactNumberInfo::None) => {
            *t = DataType::Decimal(ExactNumberInfo::PrecisionAndScale(
                BARE_NUMERIC.0 as u64,
                BARE_NUMERIC.1 as i64,
            ))
        }
        DataType::Array(
            ArrayElemTypeDef::SquareBracket(inner, _)
            | ArrayElemTypeDef::AngleBracket(inner)
            | ArrayElemTypeDef::Parenthesis(inner)
            | ArrayElemTypeDef::Qualified(inner, _),
        ) => widen_the_numeric(inner),
        _ => {}
    }
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

/// The macro a divisor is wrapped in so that a zero raises (see [`postgres_operators`]).
pub const NONZERO: &str = "sqleq_nonzero";
/// The macros a regular expression is wrapped in so that DuckDB's full match finds it anywhere in the
/// string, as Postgres's `~` does; the second also makes it case-insensitive, for `~*`.
pub const PARTIAL: &str = "sqleq_partial";
pub const IPARTIAL: &str = "sqleq_ipartial";
/// The names `power`, `pow` and `exp` are renamed to: macros that raise where Postgres's
/// `double precision` versions do (see [`postgres_operators`]).
pub const POWER: &str = "sqleq_power";
pub const EXP: &str = "sqleq_exp";

/// Make DuckDB evaluate the operators whose meaning it does not share with Postgres as Postgres does,
/// or say why it cannot: `Err` carries the reason the pair gets no verdict.
///
/// * **A zero divisor raises.** Postgres raises `division by zero` for `/`, `%` and `mod()` on every
///   numeric type; DuckDB answers NULL, `inf` or `NaN`. A trial in which one side raises is skipped,
///   so it never takes part in a comparison -- but only if it raises. So every divisor that is not a
///   non-zero literal is wrapped in [`NONZERO`], a macro that returns its argument unchanged (type
///   included) and raises on a zero.
/// * **`~` matches anywhere.** Postgres's `~`, `~*`, `!~` and `!~*` search for the pattern; DuckDB's
///   `~` is a full match and it has no `~*` at all. The pattern is wrapped in [`PARTIAL`] (or
///   [`IPARTIAL`], with `~*` becoming `~`), which turns `p` into `(?s).*(?:p).*`: a full match of that
///   is a match of `p` anywhere, with `.` matching a newline as in Postgres's default mode.
/// * **`SIMILAR TO` is refused**, and so is a regex operator under `ANY`/`ALL`. DuckDB reads a
///   `SIMILAR TO` pattern as a bare regular expression, with no `%` or `_` wildcards; translating it
///   the way Postgres does (`similar_to_escape`) is a translation of the pattern language, not a
///   rename, so a pair using it is withheld.
/// * **`LIKE` escapes with a backslash.** A Postgres `LIKE`, `ILIKE` or `NOT` either, without an
///   `ESCAPE` clause, takes `\` as its escape character, so `'\a'` matches `'a'`; DuckDB's has no
///   escape character unless given one. Each such pattern is given `ESCAPE '\'`, under which
///   DuckDB 1.5.5 matches as Postgres does, raising where Postgres raises too: on a pattern whose
///   match reaches a trailing unpaired backslash (checked against Postgres 17 over every operator
///   and a grid of escaped patterns and strings). The operator spellings `~~`, `~~*`, `!~~` and
///   `!~~*` take no `ESCAPE`, so a pair using one is withheld unless its pattern is a literal with
///   no backslash in it.
/// * **`power`, `pow` and `exp` raise as Postgres's do.** Over `double precision` Postgres raises for
///   a zero base with a negative exponent, a negative base with a fractional one, and a result that
///   overflows or underflows; DuckDB answers `inf`, `NaN` or `0`. The calls are renamed to [`POWER`]
///   and [`EXP`], which raise on those inputs and on a non-finite one. A macro cannot take the name
///   of a function DuckDB has (`crate::shim`), hence the rename; over `numeric` the functions are
///   withheld before they get here (`crate::pgtype::unmodelled`), and so is the `^` operator.
///
/// Each edit wraps an operand in a call. The operand is found by parsing it again, with sqlparser's
/// own grammar, from the token after its operator at that operator's precedence -- sqlparser's spans
/// do not cover a cast's type, so they cannot say where an operand like `b::int` ends. The result
/// must then parse to exactly the input's tree with each such operand wrapped; anything else, and a
/// statement that does not parse but whose tokens show one of these operators, withholds the pair.
pub fn postgres_operators(sql: &str) -> Result<String, String> {
    let Ok(stmts) = Parser::parse_sql(&PostgreSqlDialect {}, sql) else {
        return match significant(sql) {
            Some(toks) if toks.iter().any(|t| risky_token(&t.token)) => {
                Err("a division or a regex match in a statement that does not parse".to_string())
            }
            _ => Ok(sql.to_string()),
        };
    };
    let mut found = Operands::default();
    for st in &stmts {
        let _ = st.visit(&mut found);
    }
    if let Some(why) = found.refused {
        return Err(why);
    }
    if found.sites == 0 {
        return Ok(sql.to_string());
    }
    let unplaced =
        || "a divisor, LIKE or regex pattern whose extent could not be found".to_string();
    let toks = lex(sql).ok_or_else(unplaced)?;

    let mut edits: Vec<Edit> = Vec::new();
    for i in 0..toks.len() {
        let kind = match &toks[i].token {
            Token::Div | Token::Mod => Kind::Divisor,
            Token::Tilde | Token::ExclamationMarkTilde => Kind::Pattern { insensitive: false },
            Token::TildeAsterisk | Token::ExclamationMarkTildeAsterisk => {
                Kind::Pattern { insensitive: true }
            }
            Token::Word(w) if w.value.eq_ignore_ascii_case("mod") && w.quote_style.is_none() => {
                if let Some(e) = mod_divisor(&toks, i).map_err(|_| unplaced())? {
                    edits.extend(e);
                }
                continue;
            }
            Token::Word(w)
                if w.quote_style.is_none() && matches!(w.keyword, Keyword::LIKE | Keyword::ILIKE) =>
            {
                Kind::Like
            }
            Token::Word(w) if w.quote_style.is_none() && renamed(&w.value).is_some() => {
                let call = next_significant(&toks, i + 1)
                    .is_some_and(|k| toks[k].token == Token::LParen);
                let qualified = toks[..i]
                    .iter()
                    .rev()
                    .find(|t| !t.is_blank())
                    .is_some_and(|t| t.token == Token::Period);
                if call && !qualified {
                    let to = renamed(&w.value).expect("matched above");
                    edits.push(Edit::new(toks[i].start, toks[i].end, to, 1));
                }
                continue;
            }
            _ => continue,
        };
        if matches!(kind, Kind::Like) {
            // `LIKE ANY (..)` has no DuckDB counterpart and fails there as it is; the rest of the
            // pattern is read exactly as the parser reads it, at `LIKE`'s precedence.
            let after = next_significant(&toks, i + 1);
            if after.is_some_and(|k| {
                matches!(&toks[k].token, Token::Word(w) if matches!(w.keyword, Keyword::ANY | Keyword::ALL | Keyword::SOME))
            }) {
                continue;
            }
            let prec = Parser::new(&PostgreSqlDialect {})
                .with_tokens_with_locations(toks[i..].iter().map(Tok::with_span).collect())
                .get_next_precedence()
                .map_err(|_| unplaced())?;
            let (_, _, last) = parse_operand(&toks, i + 1, Some(prec)).ok_or_else(unplaced)?;
            let escaped = next_significant(&toks, last + 1).is_some_and(|k| {
                matches!(&toks[k].token, Token::Word(w) if w.keyword == Keyword::ESCAPE)
            });
            if !escaped {
                edits.push(Edit::new(toks[last].end, toks[last].end, " ESCAPE '\\'", 0));
            }
            continue;
        }
        // A prefix `~` is bitwise NOT, not a regex match: it follows no operand.
        if matches!(kind, Kind::Pattern { .. }) && !follows_operand(&toks, i) {
            continue;
        }
        let prec = Parser::new(&PostgreSqlDialect {})
            .with_tokens_with_locations(toks[i..].iter().map(Tok::with_span).collect())
            .get_next_precedence()
            .map_err(|_| unplaced())?;
        let (operand, first, last) = parse_operand(&toks, i + 1, Some(prec)).ok_or_else(unplaced)?;
        if matches!(kind, Kind::Divisor) && nonzero_literal(&operand) {
            continue;
        }
        let wrapper = match kind {
            Kind::Divisor => NONZERO,
            Kind::Pattern { insensitive: false } => PARTIAL,
            Kind::Pattern { insensitive: true } => IPARTIAL,
            Kind::Like => unreachable!("handled above"),
        };
        if let Kind::Pattern { insensitive: true } = kind {
            let plain = if toks[i].token == Token::TildeAsterisk { "~" } else { "!~" };
            edits.push(Edit::new(toks[i].start, toks[i].end, plain, 1));
        }
        edits.push(Edit::new(toks[first].start, toks[first].start, &format!("{wrapper}("), 2));
        edits.push(Edit::new(toks[last].end, toks[last].end, ")", 0));
    }
    let out = splice(sql, edits).ok_or_else(unplaced)?;
    let expected: Vec<Statement> = {
        let mut e = stmts.clone();
        for st in &mut e {
            let _ = VisitMut::visit(st, &mut Wrap);
        }
        e
    };
    match Parser::parse_sql(&PostgreSqlDialect {}, &out) {
        Ok(after) if denested(&after) == denested(&expected) => Ok(out),
        _ => Err(unplaced()),
    }
}

/// The name a call to `name` is renamed to, if it is one [`postgres_operators`] renames.
fn renamed(name: &str) -> Option<&'static str> {
    match name.to_ascii_lowercase().as_str() {
        "power" | "pow" => Some(POWER),
        "exp" => Some(EXP),
        _ => None,
    }
}

/// The index of the first token from `from` on that is not blank.
fn next_significant(toks: &[Tok], from: usize) -> Option<usize> {
    (from..toks.len()).find(|&k| !toks[k].is_blank())
}

/// Parse one operand starting at token `from`: at `prec` (the right operand of a binary operator) or,
/// with `None`, as a whole expression (a function argument). Returns it with the indices of its first
/// and last tokens.
fn parse_operand(toks: &[Tok], from: usize, prec: Option<u8>) -> Option<(Expr, usize, usize)> {
    let mut p = Parser::new(&PostgreSqlDialect {})
        .with_tokens_with_locations(toks.get(from..)?.iter().map(Tok::with_span).collect());
    let e = match prec {
        Some(prec) => p.parse_subexpr(prec).ok()?,
        None => p.parse_expr().ok()?,
    };
    let first = (from..toks.len()).find(|&k| !toks[k].is_blank())?;
    let last = (first..from + p.index()).rev().find(|&k| !toks[k].is_blank())?;
    Some((e, first, last))
}

/// For a `mod` token at `i`: if it is a call `mod(n, d)`, the edits that guard `d` (none when `d` is a
/// non-zero literal); `Ok(None)` when it is not a call; `Err` when it is one whose arguments cannot be
/// read.
fn mod_divisor(toks: &[Tok], i: usize) -> Result<Option<Vec<Edit>>, ()> {
    let next = |k: usize| (k..toks.len()).find(|&j| !toks[j].is_blank());
    let Some(open) = next(i + 1).filter(|&k| toks[k].token == Token::LParen) else {
        return Ok(None);
    };
    let (_, _, n_last) = parse_operand(toks, open + 1, None).ok_or(())?;
    let comma = next(n_last + 1).filter(|&k| toks[k].token == Token::Comma);
    let Some(comma) = comma else {
        return Ok(None); // not the two-argument form; the parse check settles the rest
    };
    let (d, first, last) = parse_operand(toks, comma + 1, None).ok_or(())?;
    if nonzero_literal(&d) {
        return Ok(Some(Vec::new()));
    }
    Ok(Some(vec![
        Edit::new(toks[first].start, toks[first].start, &format!("{NONZERO}("), 2),
        Edit::new(toks[last].end, toks[last].end, ")", 0),
    ]))
}

/// Whether the token before `i` ends an operand, so that a `~` at `i` is the binary regex operator
/// rather than prefix bitwise NOT.
fn follows_operand(toks: &[Tok], i: usize) -> bool {
    let Some(prev) = toks[..i].iter().rev().find(|t| !t.is_blank()) else {
        return false;
    };
    match &prev.token {
        Token::RParen | Token::RBracket | Token::Number(..) | Token::Placeholder(_) => true,
        Token::Word(w) => {
            w.quote_style.is_some()
                || !matches!(
                    w.keyword,
                    Keyword::SELECT
                        | Keyword::WHERE
                        | Keyword::AND
                        | Keyword::OR
                        | Keyword::NOT
                        | Keyword::WHEN
                        | Keyword::THEN
                        | Keyword::ELSE
                        | Keyword::ON
                        | Keyword::HAVING
                        | Keyword::BY
                        | Keyword::SET
                        | Keyword::RETURNING
                        | Keyword::IN
                        | Keyword::IS
                        | Keyword::LIKE
                        | Keyword::CASE
                        | Keyword::VALUES
                )
        }
        t => is_literal_token(t),
    }
}

/// The tokens that betray one of [`postgres_operators`]'s operators in a statement that did not parse.
fn risky_token(t: &Token) -> bool {
    match t {
        Token::Div
        | Token::Mod
        | Token::Tilde
        | Token::TildeAsterisk
        | Token::ExclamationMarkTilde
        | Token::ExclamationMarkTildeAsterisk
        | Token::DoubleTilde
        | Token::DoubleTildeAsterisk
        | Token::ExclamationMarkDoubleTilde
        | Token::ExclamationMarkDoubleTildeAsterisk => true,
        Token::Word(w) => {
            w.quote_style.is_none()
                && (matches!(w.keyword, Keyword::SIMILAR | Keyword::LIKE | Keyword::ILIKE)
                    || w.value.eq_ignore_ascii_case("mod")
                    || renamed(&w.value).is_some())
        }
        _ => false,
    }
}

#[derive(Clone, Copy, Debug)]
enum Kind {
    Divisor,
    Pattern { insensitive: bool },
    Like,
}

/// What [`postgres_operators`] finds on the parse: how many operands it has to wrap, and a reason
/// to withhold the pair instead.
#[derive(Default)]
struct Operands {
    sites: usize,
    refused: Option<String>,
}

/// A literal the divisor is known to be non-zero for, which needs no guard.
fn nonzero_literal(e: &Expr) -> bool {
    match e {
        Expr::Nested(inner) => nonzero_literal(inner),
        Expr::UnaryOp {
            op: UnaryOperator::Minus | UnaryOperator::Plus,
            expr,
        } => nonzero_literal(expr),
        Expr::Value(v) => {
            matches!(&v.value, Value::Number(n, _) if n.parse::<f64>().is_ok_and(|x| x != 0.0))
        }
        _ => false,
    }
}

/// The bare (last-part, lower-cased) name of a function call, if `e` is one.
fn call_name(e: &Expr) -> Option<String> {
    let Expr::Function(f) = e else { return None };
    f.name.0.last().and_then(|p| match p {
        ObjectNamePart::Identifier(id) => Some(id.value.to_lowercase()),
        _ => None,
    })
}

/// The plain positional arguments of a call, if it has nothing but those.
fn plain_args(e: &Expr) -> Option<Vec<&Expr>> {
    let Expr::Function(f) = e else { return None };
    let FunctionArguments::List(list) = &f.args else {
        return None;
    };
    if list.duplicate_treatment.is_some() || !list.clauses.is_empty() {
        return None;
    }
    list.args
        .iter()
        .map(|a| match a {
            FunctionArg::Unnamed(FunctionArgExpr::Expr(x)) => Some(x),
            _ => None,
        })
        .collect()
}

/// Whether a `LIKE` is one [`postgres_operators`] gives an escape character: no `ESCAPE` of its own,
/// and not `LIKE ANY` or `LIKE ALL`, which sqlparser reads as a call named `ALL`.
fn unescaped_like(e: &Expr) -> bool {
    match e {
        Expr::Like {
            any: false,
            escape_char: None,
            pattern,
            ..
        }
        | Expr::ILike {
            any: false,
            escape_char: None,
            pattern,
            ..
        } => !matches!(call_name(pattern).as_deref(), Some("all" | "any" | "some")),
        _ => false,
    }
}

/// Whether `e` is a call [`postgres_operators`] renames: `power`, `pow` or `exp`, unqualified.
fn renamed_call(e: &Expr) -> bool {
    match e {
        Expr::Function(f) => matches!(
            f.name.0.as_slice(),
            [ObjectNamePart::Identifier(id)] if id.quote_style.is_none() && renamed(&id.value).is_some()
        ),
        _ => false,
    }
}

fn is_like_operator(op: &BinaryOperator) -> bool {
    matches!(
        op,
        BinaryOperator::PGLikeMatch
            | BinaryOperator::PGILikeMatch
            | BinaryOperator::PGNotLikeMatch
            | BinaryOperator::PGNotILikeMatch
    )
}

/// A string literal with no backslash in it: a pattern on which an escape character changes nothing.
fn plain_pattern(e: &Expr) -> bool {
    match e {
        Expr::Nested(inner) => plain_pattern(inner),
        Expr::Value(v) => matches!(&v.value, Value::SingleQuotedString(s) if !s.contains('\\')),
        _ => false,
    }
}

fn is_regex(op: &BinaryOperator) -> bool {
    matches!(
        op,
        BinaryOperator::PGRegexMatch
            | BinaryOperator::PGRegexIMatch
            | BinaryOperator::PGRegexNotMatch
            | BinaryOperator::PGRegexNotIMatch
    )
}

impl Visitor for Operands {
    type Break = ();

    fn pre_visit_expr(&mut self, e: &Expr) -> ControlFlow<()> {
        match e {
            Expr::SimilarTo { .. } => {
                self.refused
                    .get_or_insert("SIMILAR TO (its pattern language has no DuckDB counterpart)".into());
            }
            Expr::AnyOp { compare_op, .. } | Expr::AllOp { compare_op, .. }
                if is_regex(compare_op) =>
            {
                self.refused
                    .get_or_insert("a regex operator under ANY/ALL".into());
            }
            Expr::BinaryOp { op, right, .. } if is_like_operator(op) && !plain_pattern(right) => {
                self.refused.get_or_insert(
                    "a ~~ operator (Postgres's escapes with a backslash, DuckDB's cannot)".into(),
                );
            }
            e if unescaped_like(e) || renamed_call(e) => self.sites += 1,
            // `pg_catalog.power(..)` or `"exp"(..)`: Postgres's function all the same, but not a
            // spelling the rename reaches.
            Expr::Function(_) if call_name(e).is_some_and(|n| renamed(&n).is_some()) => {
                self.refused
                    .get_or_insert("a schema-qualified or quoted power or exp".into());
            }
            Expr::BinaryOp { op, right, .. } => {
                let divides = matches!(op, BinaryOperator::Divide | BinaryOperator::Modulo);
                if (divides && !nonzero_literal(right)) || is_regex(op) {
                    self.sites += 1;
                }
            }
            e if call_name(e).as_deref() == Some("mod") => {
                if let Some([_, d]) = plain_args(e).as_deref() {
                    if !nonzero_literal(d) {
                        self.sites += 1;
                    }
                }
            }
            _ => {}
        }
        ControlFlow::Continue(())
    }
}

/// A call `name(arg)`, as the parser builds one from that text.
fn call(name: &str, arg: Expr) -> Expr {
    let template = format!("SELECT {name}(1)");
    let mut stmts = Parser::parse_sql(&PostgreSqlDialect {}, &template).expect("a call parses");
    let Statement::Query(q) = &mut stmts[0] else {
        unreachable!("a SELECT is a query")
    };
    let sqlparser::ast::SetExpr::Select(sel) = &mut *q.body else {
        unreachable!("a SELECT has a select body")
    };
    let SelectItem::UnnamedExpr(Expr::Function(f)) = &mut sel.projection[0] else {
        unreachable!("the projection is the call")
    };
    if let FunctionArguments::List(list) = &mut f.args {
        list.args[0] = FunctionArg::Unnamed(FunctionArgExpr::Expr(arg));
    }
    sel.projection.remove(0).into_expr()
}

trait IntoExpr {
    fn into_expr(self) -> Expr;
}

impl IntoExpr for SelectItem {
    fn into_expr(self) -> Expr {
        match self {
            SelectItem::UnnamedExpr(e) => e,
            _ => unreachable!("only called on an unnamed expression"),
        }
    }
}

/// The parse [`postgres_operators`] claims its output has: each guarded operand wrapped, in the same
/// places its token walk wraps them. Post-order, so an inner operand is wrapped before the operator
/// that contains it is looked at.
struct Wrap;

impl VisitorMut for Wrap {
    type Break = ();

    fn post_visit_expr(&mut self, e: &mut Expr) -> ControlFlow<()> {
        if unescaped_like(e) {
            if let Expr::Like { escape_char, .. } | Expr::ILike { escape_char, .. } = e {
                *escape_char = Some(Box::new(Expr::Value(
                    Value::SingleQuotedString("\\".to_string()).with_empty_span(),
                )));
            }
            return ControlFlow::Continue(());
        }
        if renamed_call(e) {
            let to = call_name(e).and_then(|n| renamed(&n)).expect("checked above");
            if let Expr::Function(f) = e {
                f.name = ObjectName(vec![ObjectNamePart::Identifier(Ident::new(to))]);
            }
            return ControlFlow::Continue(());
        }
        if call_name(e).as_deref() == Some("mod") {
            if let Expr::Function(f) = e {
                if let FunctionArguments::List(list) = &mut f.args {
                    if list.args.len() == 2 && list.duplicate_treatment.is_none() && list.clauses.is_empty() {
                        if let FunctionArg::Unnamed(FunctionArgExpr::Expr(d)) = &mut list.args[1] {
                            if !nonzero_literal(d) {
                                *d = call(NONZERO, d.clone());
                            }
                        }
                    }
                }
            }
            return ControlFlow::Continue(());
        }
        if let Expr::BinaryOp { op, right, .. } = e {
            match op {
                BinaryOperator::Divide | BinaryOperator::Modulo if !nonzero_literal(right) => {
                    **right = call(NONZERO, (**right).clone());
                }
                BinaryOperator::PGRegexMatch | BinaryOperator::PGRegexNotMatch => {
                    **right = call(PARTIAL, (**right).clone());
                }
                BinaryOperator::PGRegexIMatch => {
                    *op = BinaryOperator::PGRegexMatch;
                    **right = call(IPARTIAL, (**right).clone());
                }
                BinaryOperator::PGRegexNotIMatch => {
                    *op = BinaryOperator::PGRegexNotMatch;
                    **right = call(IPARTIAL, (**right).clone());
                }
                _ => {}
            }
        }
        ControlFlow::Continue(())
    }
}

/// A replacement of `start..end` with `text`; an insertion when the range is empty. `rank` orders
/// edits that start at one offset: closing insertions (0), then replacements (1), then opening
/// insertions (2) -- an operand that ends where another begins encloses neither.
struct Edit {
    start: usize,
    end: usize,
    text: String,
    rank: u8,
}

impl Edit {
    fn new(start: usize, end: usize, text: &str, rank: u8) -> Self {
        Edit {
            start,
            end,
            text: text.to_string(),
            rank,
        }
    }
}

/// Apply non-overlapping edits; `None` if two replacements overlap or an edit is out of bounds.
fn splice(sql: &str, mut edits: Vec<Edit>) -> Option<String> {
    edits.sort_by_key(|e| (e.start, e.rank));
    let mut out = String::with_capacity(sql.len() + 16 * edits.len());
    let mut cur = 0;
    for e in edits {
        if e.start < cur || e.end > sql.len() || !sql.is_char_boundary(e.start) {
            return None;
        }
        out.push_str(&sql[cur..e.start]);
        out.push_str(&e.text);
        cur = e.end;
    }
    out.push_str(&sql[cur..]);
    Some(out)
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

    /// `LIKE` gets Postgres's escape character, and `power`/`exp` the macros that raise as
    /// Postgres's do (issue #89).
    mod like_and_power {
        use super::super::postgres_operators;

        fn ops(sql: &str) -> Result<String, String> {
            postgres_operators(sql)
        }

        #[test]
        fn a_like_without_an_escape_clause_gets_a_backslash() {
            for (sql, want) in [
                (
                    "SELECT 1 FROM t WHERE s LIKE 'a'",
                    r"SELECT 1 FROM t WHERE s LIKE 'a' ESCAPE '\'",
                ),
                (
                    "SELECT 1 FROM t WHERE s NOT ILIKE $1 AND x",
                    r"SELECT 1 FROM t WHERE s NOT ILIKE $1 ESCAPE '\' AND x",
                ),
                (
                    "SELECT s LIKE 'a' || b FROM t",
                    r"SELECT s LIKE 'a' || b ESCAPE '\' FROM t",
                ),
                (
                    "SELECT 1 FROM t WHERE s LIKE 'a' AND s LIKE lower(b)",
                    r"SELECT 1 FROM t WHERE s LIKE 'a' ESCAPE '\' AND s LIKE lower(b) ESCAPE '\'",
                ),
            ] {
                assert_eq!(ops(sql).as_deref(), Ok(want), "{sql}");
            }
            // An escape of its own, and the quantified forms, are left alone.
            for sql in [
                "SELECT 1 FROM t WHERE s LIKE 'a!%' ESCAPE '!'",
                "SELECT 1 FROM t WHERE s LIKE ANY (ARRAY['a', 'b'])",
                "SELECT 1 FROM t WHERE s LIKE ALL (ARRAY['a', 'b'])",
            ] {
                assert_eq!(ops(sql).as_deref(), Ok(sql), "{sql}");
            }
        }

        #[test]
        fn the_like_operators_are_withheld_unless_nothing_is_escaped() {
            assert!(ops(r"SELECT 1 FROM t WHERE s ~~ '\a'").is_err());
            assert!(ops("SELECT 1 FROM t WHERE s !~~* $1").is_err());
            let plain = "SELECT 1 FROM t WHERE s ~~ 'a%'";
            assert_eq!(ops(plain).as_deref(), Ok(plain));
        }

        #[test]
        fn power_and_exp_are_renamed_to_macros_that_raise() {
            for (sql, want) in [
                (
                    "SELECT POWER(a, -1), pow(a, 2) FROM t",
                    "SELECT sqleq_power(a, -1), sqleq_power(a, 2) FROM t",
                ),
                (
                    "SELECT 1 FROM t WHERE exp(a / b) > 0",
                    "SELECT 1 FROM t WHERE sqleq_exp(a / sqleq_nonzero(b)) > 0",
                ),
            ] {
                assert_eq!(ops(sql).as_deref(), Ok(want), "{sql}");
            }
            // A qualified or quoted call is not renamed, so it is withheld; a column named alike is
            // no call.
            assert!(ops("SELECT pg_catalog.power(a, 2) FROM t").is_err());
            assert!(ops(r#"SELECT "exp"(a) FROM t"#).is_err());
            let column = "SELECT exp FROM t WHERE power > 1";
            assert_eq!(ops(column).as_deref(), Ok(column));
        }
    }
}
