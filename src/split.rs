// Unless explicitly stated otherwise all files in this repository are licensed under the
// Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/).
// Copyright 2026-Present Datadog, Inc.

//! SQL text cut where Postgres cuts it: a script into statements, at each `;` that ends one.
//!
//! Which `;` ends a statement depends on every way Postgres quotes text: `'…'` with `''` doubled,
//! `E'…'` with backslash escapes, `"…"` identifiers, `$$…$$` and `$tag$…$tag$` bodies, `--`
//! comments, and `/* … */` comments, which nest. A hand-written scanner has to get every one of
//! them right, and the three this crate, `sqleq-check` and `sqleq-fuzz` each had missed some
//! (issue #93). sqlparser's tokenizer under the Postgres dialect reads all of them as Postgres does,
//! and it is what reads the pair, so the cuts are made on its tokens. Above the tokens, a `;` ends a
//! statement where psql ends one: outside parentheses (a rule's actions are a parenthesised list of
//! statements) and outside the `BEGIN ATOMIC … END` body of a `CREATE FUNCTION` or `CREATE
//! PROCEDURE`.
//!
//! Text the tokenizer rejects is an error, never a guess. Split by a cruder rule, one statement
//! becomes two, and the pieces can be read as `x` against `x`, as a schema the DDL does not build,
//! or as a refutation. So is text it would read otherwise than Postgres does, where that moves a
//! cut (`unlike_postgres`).
//!
//! `sqleq-fuzz` keeps a copy built the same way, since it does not depend on this crate. The table
//! `tests/data/statement_splits.json` holds both to the same cuts.

use sqlparser::tokenizer::{Location, Token, Tokenizer};

/// Why text could not be cut into statements: the tokenizer rejects it (an unterminated literal,
/// quoted identifier, dollar-quoted body or comment, or an escape it does not read), or reads it
/// otherwise than Postgres does, in a way that moves a cut.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SplitError(String);

impl std::fmt::Display for SplitError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for SplitError {}

/// The statements of `sql`, in order, each as the text from its first token to its last: the
/// comments around a statement are not part of it, the ones inside it are. A `;` with nothing but
/// comments or blanks before it ends no statement, and the text after the last `;` is a statement
/// unless it holds nothing either. `Err` as [`SplitError`] says.
///
/// ```
/// let sql = "SELECT 'a;b' -- c;\n; /* d; /* e; */ */ SELECT $$f; g$$, E'h\\';i';";
/// let st = sqleq_frontend::split::statements(sql).unwrap();
/// assert_eq!(st, ["SELECT 'a;b'", "SELECT $$f; g$$, E'h\\';i'"]);
/// assert!(sqleq_frontend::split::statements("SELECT 'unterminated; SELECT 1").is_err());
/// ```
pub fn statements(sql: &str) -> Result<Vec<&str>, SplitError> {
    let toks = lex(sql)?;
    Ok(cut(&toks).into_iter().map(|(s, e)| &sql[s..e]).collect())
}

/// `sql` with each run of whitespace and comments between its tokens made one space, and none at
/// either end; literals and quoted names are kept as written. Two texts with the same compact form
/// are the same tokens in the same order. `Err` as [`SplitError`] says.
///
/// ```
/// let c = sqleq_frontend::split::compact("select /* a */ 'x  y'\n  from\tt -- b").unwrap();
/// assert_eq!(c, "select 'x  y' from t");
/// ```
pub fn compact(sql: &str) -> Result<String, SplitError> {
    let mut out = String::with_capacity(sql.len());
    let mut gap = false;
    for t in lex(sql)? {
        if t.is_blank() {
            gap = true;
            continue;
        }
        if gap && !out.is_empty() {
            out.push(' ');
        }
        gap = false;
        out.push_str(&sql[t.start..t.end]);
    }
    Ok(out)
}

/// One token of a statement and the byte range `start..end` it spans.
#[derive(Clone, Debug)]
pub(crate) struct Tok {
    pub token: Token,
    pub start: usize,
    pub end: usize,
}

impl Tok {
    /// Whitespace or a comment: what separates tokens rather than being one.
    fn is_blank(&self) -> bool {
        matches!(self.token, Token::Whitespace(_))
    }
}

/// `sql` as the tokenizer reads it under [`crate::DIALECT`], each token with its byte range.
pub(crate) fn lex(sql: &str) -> Result<Vec<Tok>, SplitError> {
    let tokens = Tokenizer::new(&crate::DIALECT, sql)
        .tokenize_with_location()
        .map_err(|e| SplitError(e.to_string()))?;
    let mut cur = Cursor { sql, line: 1, col: 1, byte: 0 };
    let mut out = Vec::with_capacity(tokens.len());
    for t in tokens {
        if matches!(t.token, Token::EOF) {
            continue;
        }
        let misplaced = || SplitError(format!("cannot place the token {} in the text", t.token));
        let start = cur.seek(t.span.start).ok_or_else(misplaced)?;
        let end = cur.seek(t.span.end).ok_or_else(misplaced)?;
        out.push(Tok { token: t.token, start, end });
    }
    if let Some(why) = out.iter().find_map(|t| unlike_postgres(&t.token, &sql[t.start..t.end])) {
        return Err(SplitError(why));
    }
    Ok(out)
}

/// Why `t`, spelled `text`, may not be what Postgres's lexer reads there, in a way that moves where a
/// literal or a comment ends, and so where a statement does; `None` if it is.
///
/// Each is text Postgres rejects or reads otherwise, and sqlparser reads on:
/// - an operator that runs into `--` or `/*`: Postgres ends the operator there and reads a comment
///   (`a >-- x; y` is `a >` and a comment), sqlparser makes it one operator and reads the `;`;
/// - a backslash in `X'…'`: no escape to Postgres, an escaped quote to sqlparser;
/// - a dollar-quote tag that starts with a digit, which Postgres reads as a parameter (`$1$`);
/// - any other `$` placeholder than `$` and digits, which Postgres may read as the start of a
///   dollar-quoted body whose tag sqlparser does not take for one (`$€$`), or rejects.
fn unlike_postgres(t: &Token, text: &str) -> Option<String> {
    match t {
        Token::CustomBinaryOperator(op) if op.contains("--") || op.contains("/*") => {
            Some(format!("the operator `{op}` runs into a comment, where Postgres ends it"))
        }
        Token::HexStringLiteral(_) if text.contains('\\') => {
            Some("a backslash in an X'…' string, which Postgres does not read as an escape".to_string())
        }
        Token::DollarQuotedString(d) => {
            let tag = d.tag.as_deref().filter(|g| g.starts_with(|c: char| c.is_ascii_digit()))?;
            Some(format!("the dollar-quote tag `${tag}$`, which Postgres reads as a parameter"))
        }
        Token::Placeholder(p)
            if p.strip_prefix('$').is_some_and(|n| n.is_empty() || !n.bytes().all(|b| b.is_ascii_digit())) =>
        {
            Some(format!("`{p}`, which Postgres does not read as a parameter"))
        }
        _ => None,
    }
}

/// The byte range of each statement of `toks`, from its first token that is not blank to its last.
fn cut(toks: &[Tok]) -> Vec<(usize, usize)> {
    let mut out = Vec::new();
    let mut st = Statement::default();
    for t in toks.iter().filter(|t| !t.is_blank()) {
        if matches!(t.token, Token::SemiColon) && st.ends_at_semicolon() {
            out.extend(st.span);
            st = Statement::default();
            continue;
        }
        st.read(&t.token);
        st.span = Some(st.span.map_or((t.start, t.end), |(s, _)| (s, t.end)));
    }
    out.extend(st.span);
    out
}

/// What psql tracks about the statement it is reading to tell whether a `;` ends it.
#[derive(Default)]
struct Statement {
    span: Option<(usize, usize)>,
    /// Open parentheses.
    depth: usize,
    /// Open `BEGIN` (and, inside one, `CASE`) blocks of a routine body.
    begins: usize,
    /// How many words the statement has had, and the first letter of each of its first four that
    /// is `create`, `or`, `replace`, `function` or `procedure` (0 for any other word).
    words: usize,
    heads: [u8; 4],
}

impl Statement {
    /// psql's rule (`psqlscan.l`): outside parentheses and outside a routine's `BEGIN … END`.
    fn ends_at_semicolon(&self) -> bool {
        self.depth == 0 && self.begins == 0
    }

    fn read(&mut self, t: &Token) {
        match t {
            Token::LParen => self.depth += 1,
            Token::RParen => self.depth = self.depth.saturating_sub(1),
            // A quoted identifier is no keyword, to psql as to Postgres.
            Token::Word(w) if w.quote_style.is_none() => {
                let w = w.value.to_ascii_lowercase();
                if let Some(h) = self.heads.get_mut(self.words) {
                    *h = match w.as_str() {
                        "create" | "or" | "replace" | "function" | "procedure" => w.as_bytes()[0],
                        _ => 0,
                    };
                }
                self.words += 1;
                // `CREATE [OR REPLACE] FUNCTION|PROCEDURE`, whose body may be `BEGIN ATOMIC … END`
                // with a `;` after each statement in it.
                let h = self.heads;
                let routine = h[0] == b'c'
                    && (matches!(h[1], b'f' | b'p') || (h[1] == b'o' && h[2] == b'r' && matches!(h[3], b'f' | b'p')));
                if routine && self.depth == 0 {
                    match w.as_str() {
                        "begin" => self.begins += 1,
                        // `CASE` ends in `END` too; it matters only inside a `BEGIN`.
                        "case" if self.begins > 0 => self.begins += 1,
                        "end" => self.begins = self.begins.saturating_sub(1),
                        _ => {}
                    }
                }
            }
            _ => {}
        }
    }
}

/// Converts the tokenizer's (line, character column) locations to byte offsets, in one forward
/// pass: tokens arrive in source order.
struct Cursor<'a> {
    sql: &'a str,
    line: u64,
    col: u64,
    byte: usize,
}

impl Cursor<'_> {
    /// Advance to `l` and return its byte offset; `None` for a location behind the cursor or past
    /// the end.
    fn seek(&mut self, l: Location) -> Option<usize> {
        if (l.line, l.column) < (self.line, self.col) {
            return None;
        }
        while (self.line, self.col) < (l.line, l.column) {
            let c = self.sql[self.byte..].chars().next()?;
            self.byte += c.len_utf8();
            if c == '\n' {
                self.line += 1;
                self.col = 1;
            } else {
                self.col += 1;
            }
        }
        Some(self.byte)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The inputs `tests/data/statement_splits.json` holds, and how each is cut, or that it is an
    /// error. `sqleq-fuzz`'s splitter, a copy built the same way, is held to the same table.
    #[test]
    fn the_shared_table_of_cuts() {
        let cases: Vec<serde_json::Value> =
            serde_json::from_str(include_str!("../tests/data/statement_splits.json")).unwrap();
        assert!(cases.len() > 30);
        for c in &cases {
            let (why, sql) = (c["why"].as_str().unwrap(), c["sql"].as_str().unwrap());
            match (statements(sql), c.get("statements")) {
                (Ok(got), Some(want)) => {
                    let want: Vec<&str> = want.as_array().unwrap().iter().map(|s| s.as_str().unwrap()).collect();
                    assert_eq!(got, want, "{why}: {sql:?}");
                }
                (Err(_), None) if c["error"] == true => {}
                (got, _) => panic!("{why}: {sql:?} gave {got:?}"),
            }
        }
    }

    #[test]
    fn compact_text_keeps_literals_and_drops_comments() {
        assert_eq!(compact("  select   1\n  +\t2 ").unwrap(), "select 1 + 2");
        assert_eq!(compact("select/* a; */1 -- b\n").unwrap(), "select 1");
        // A literal's spaces are its value: `'a  b'` is not `'a b'`.
        assert_eq!(compact("select 'a  b',  $$c  d$$").unwrap(), "select 'a  b', $$c  d$$");
        // A comment between two literals stays a gap, or the two would read back as one.
        assert_eq!(compact("select 'a'/**/'b'").unwrap(), "select 'a' 'b'");
        assert!(compact("select 'a").is_err());
    }

    /// An error names what the tokenizer could not read.
    #[test]
    fn an_error_says_why() {
        let e = statements("SELECT a >-- x; y\n 1").unwrap_err().to_string();
        assert!(e.contains("`>--`"), "{e}");
        let e = statements("SELECT 'a").unwrap_err().to_string();
        assert!(e.contains("Unterminated string literal"), "{e}");
    }
}
