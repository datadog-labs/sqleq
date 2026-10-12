// Unless explicitly stated otherwise all files in this repository are licensed under the
// Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/).
// Copyright 2026-Present Datadog, Inc.

//! The statement as Postgres's lexer sees it: tokens with their byte ranges.
//!
//! A regular expression over raw SQL cannot tell `$1` from the same two characters inside a string
//! literal, or `now()` in a query from `'now()'` in a comment. The tokenizer can, so the questions
//! whose answers decide a verdict — which placeholders a statement has, which functions it calls,
//! where a clock is read — are answered here or on the parse, and the remaining text-level patterns
//! run over [`mask`]ed text, in which literals and comments no longer hold anything to match.

use sqlparser::dialect::PostgreSqlDialect;
use sqlparser::tokenizer::{Location, Span, Token, TokenWithSpan, Tokenizer, Whitespace};

/// One token and the byte range `start..end` it spans in the statement.
#[derive(Clone, Debug)]
pub struct Tok {
    pub token: Token,
    pub start: usize,
    pub end: usize,
    /// The tokenizer's own location, for handing the token back to a parser.
    pub span: Span,
}

impl Tok {
    /// The token as the parser takes it.
    pub fn with_span(&self) -> TokenWithSpan {
        TokenWithSpan {
            token: self.token.clone(),
            span: self.span,
        }
    }

    /// Whitespace and comments: what separates tokens rather than being one.
    pub fn is_blank(&self) -> bool {
        matches!(self.token, Token::Whitespace(_))
    }

    /// A literal whose *contents* are data, never SQL.
    fn is_literal(&self) -> bool {
        is_literal_token(&self.token)
    }

    fn is_comment(&self) -> bool {
        matches!(
            self.token,
            Token::Whitespace(Whitespace::SingleLineComment { .. })
                | Token::Whitespace(Whitespace::MultiLineComment(_))
        )
    }
}

/// Every quoted-string form the tokenizer knows: a literal whose contents are data, never SQL.
pub fn is_literal_token(t: &Token) -> bool {
    matches!(
        t,
        Token::SingleQuotedString(_)
            | Token::DoubleQuotedString(_)
            | Token::TripleSingleQuotedString(_)
            | Token::TripleDoubleQuotedString(_)
            | Token::DollarQuotedString(_)
            | Token::SingleQuotedByteStringLiteral(_)
            | Token::DoubleQuotedByteStringLiteral(_)
            | Token::TripleSingleQuotedByteStringLiteral(_)
            | Token::TripleDoubleQuotedByteStringLiteral(_)
            | Token::SingleQuotedRawStringLiteral(_)
            | Token::DoubleQuotedRawStringLiteral(_)
            | Token::TripleSingleQuotedRawStringLiteral(_)
            | Token::TripleDoubleQuotedRawStringLiteral(_)
            | Token::NationalStringLiteral(_)
            | Token::QuoteDelimitedStringLiteral(_)
            | Token::NationalQuoteDelimitedStringLiteral(_)
            | Token::EscapedStringLiteral(_)
            | Token::UnicodeStringLiteral(_)
            | Token::HexStringLiteral(_)
    )
}

/// Tokenize `sql` with the crate's dialect, every token carrying its byte range, or `None` if the
/// tokenizer rejects it (an unterminated literal, say).
pub fn lex(sql: &str) -> Option<Vec<Tok>> {
    lex_or_why(sql).ok()
}

/// [`lex`], with the tokenizer's message when it rejects `sql`.
///
/// The tokenizer reports 1-based line and *character* columns; tokens arrive in source order, so
/// one forward walk converts them all.
pub fn lex_or_why(sql: &str) -> Result<Vec<Tok>, String> {
    let tokens = Tokenizer::new(&PostgreSqlDialect {}, sql)
        .tokenize_with_location()
        .map_err(|e| e.to_string())?;
    let mut cur = Cursor::new(sql);
    let mut out = Vec::with_capacity(tokens.len());
    for t in tokens {
        if matches!(t.token, Token::EOF) {
            continue;
        }
        let misplaced = || format!("cannot place the token {} in the text", t.token);
        let start = cur.seek(t.span.start).ok_or_else(misplaced)?;
        let end = cur.seek(t.span.end).ok_or_else(misplaced)?;
        out.push(Tok {
            token: t.token,
            start,
            end,
            span: t.span,
        });
    }
    Ok(out)
}

/// The tokens that are not whitespace or comments.
pub fn significant(sql: &str) -> Option<Vec<Tok>> {
    lex(sql).map(|ts| ts.into_iter().filter(|t| !t.is_blank()).collect())
}

/// `sql` with the contents of every string literal and every comment replaced by spaces, byte for
/// byte, so a byte offset into the result is the same offset into `sql`. The quotes of a literal
/// stay, so a pattern that expects a literal in some position still finds one there. `None` if the
/// tokenizer rejects the statement.
pub fn mask(sql: &str) -> Option<String> {
    let mut bytes = sql.as_bytes().to_vec();
    for t in lex(sql)? {
        let (from, to) = if t.is_literal() {
            // Keep the delimiting byte at each end; everything between is data.
            (t.start + 1, t.end.saturating_sub(1))
        } else if t.is_comment() {
            (t.start, t.end)
        } else {
            continue;
        };
        for b in bytes.iter_mut().take(to).skip(from) {
            *b = b' ';
        }
    }
    // Only ASCII spaces were written, and only over whole literal or comment bodies, whose bytes
    // are entire characters; any multi-byte character inside one became that many spaces.
    String::from_utf8(bytes).ok()
}

/// Converts the tokenizer's (line, char column) locations to byte offsets, in one forward pass.
struct Cursor<'a> {
    sql: &'a str,
    line: u64,
    col: u64,
    byte: usize,
}

impl<'a> Cursor<'a> {
    fn new(sql: &'a str) -> Self {
        Cursor {
            sql,
            line: 1,
            col: 1,
            byte: 0,
        }
    }

    /// Advance to `l` and return its byte offset; `None` for a location behind the cursor, past the
    /// end, or the `0:0` "no location" sqlparser reports for a synthetic token.
    fn seek(&mut self, l: Location) -> Option<usize> {
        if l.line == 0 || l.column == 0 || (l.line, l.column) < (self.line, self.col) {
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

/// [`lex_or_why`], refusing the tokens where sqlparser's tokenizer and Postgres's lexer may read
/// different text as a literal or a comment, and so would cut a statement or a list elsewhere:
/// - an operator that runs into `--` or `/*`: Postgres ends the operator there and reads a comment
///   (`a >-- x; y` is `a >` and a comment), sqlparser makes it one operator and reads the `;`;
/// - a backslash in `X'…'`: no escape to Postgres, an escaped quote to sqlparser;
/// - a dollar-quote tag that starts with a digit, which Postgres reads as a parameter (`$1$`);
/// - any other `$` placeholder than `$` and digits, which Postgres may read as the start of a
///   dollar-quoted body whose tag sqlparser does not take for one (`$€$`), or rejects.
pub fn lex_as_postgres(sql: &str) -> Result<Vec<Tok>, String> {
    let toks = lex_or_why(sql)?;
    for t in &toks {
        let text = &sql[t.start..t.end];
        let why = match &t.token {
            Token::CustomBinaryOperator(op) if op.contains("--") || op.contains("/*") => {
                format!("the operator `{op}` runs into a comment, where Postgres ends it")
            }
            Token::HexStringLiteral(_) if text.contains('\\') => {
                "a backslash in an X'…' string, which Postgres does not read as an escape"
                    .to_string()
            }
            Token::DollarQuotedString(d) => match d.tag.as_deref() {
                Some(tag) if tag.starts_with(|c: char| c.is_ascii_digit()) => {
                    format!("the dollar-quote tag `${tag}$`, which Postgres reads as a parameter")
                }
                _ => continue,
            },
            Token::Placeholder(p)
                if p.strip_prefix('$')
                    .is_some_and(|n| n.is_empty() || !n.bytes().all(|b| b.is_ascii_digit())) =>
            {
                format!("`{p}`, which Postgres does not read as a parameter")
            }
            _ => continue,
        };
        return Err(why);
    }
    Ok(toks)
}

/// The statements of `src`, in order, cut where Postgres cuts them: at each `;` the tokenizer reads
/// as one, so a `;` inside a string literal (`E'…'` with its backslash escapes too), a quoted
/// identifier, a dollar-quoted body or a comment (`/* … */` ones nest) ends nothing; and, as psql
/// does, only at a `;` outside parentheses (a rule's actions are a parenthesised list of statements)
/// and outside the `BEGIN ATOMIC … END` body of a `CREATE FUNCTION` or `CREATE PROCEDURE`.
///
/// Each statement is the text from its first token to its last, so a caller's `CREATE` test sees
/// the keyword rather than the `--` line above it, and text that is only comments is no statement.
/// `Err` with the tokenizer's message when it rejects `src`, never a guess: cut by a cruder rule,
/// one statement becomes two, and a refutation can be made of the pieces.
///
/// File mode splits a pair file with it, [`crate::schema`] the DDL it cannot parse whole, and
/// `PREPARE` is handed a side only when it is one statement by it.
///
/// The frontend has a splitter built the same way (`sqleq_frontend::split`). This one is a copy on
/// purpose: sqleq-fuzz is an oracle independent of the frontend, and does not depend on it. The
/// table `tests/data/statement_splits.json` holds the two to the same cuts.
pub fn split_statements(src: &str) -> Result<Vec<&str>, String> {
    let toks = lex_as_postgres(src)?;
    let mut out = Vec::new();
    let mut st = Statement::default();
    for t in toks.iter().filter(|t| !t.is_blank()) {
        if matches!(t.token, Token::SemiColon) && st.depth == 0 && st.begins == 0 {
            out.extend(st.span.map(|(s, e)| &src[s..e]));
            st = Statement::default();
            continue;
        }
        st.read(&t.token);
        st.span = Some(st.span.map_or((t.start, t.end), |(s, _)| (s, t.end)));
    }
    out.extend(st.span.map(|(s, e)| &src[s..e]));
    Ok(out)
}

/// What psql (`psqlscan.l`) tracks about the statement it is reading, to tell whether a `;` ends it.
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
                    && (matches!(h[1], b'f' | b'p')
                        || (h[1] == b'o' && h[2] == b'r' && matches!(h[3], b'f' | b'p')));
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn byte_ranges_cover_each_token_exactly() {
        let sql = "SELECT 'é$1', \"x\" -- c $2\nFROM t WHERE a = $3";
        let toks = lex(sql).unwrap();
        for t in &toks {
            let text = &sql[t.start..t.end];
            match &t.token {
                Token::Placeholder(p) => assert_eq!(text, p),
                Token::SingleQuotedString(s) => assert_eq!(text, format!("'{s}'")),
                _ => {}
            }
        }
        let ph: Vec<&str> = toks
            .iter()
            .filter(|t| matches!(t.token, Token::Placeholder(_)))
            .map(|t| &sql[t.start..t.end])
            .collect();
        assert_eq!(ph, vec!["$3"]);
    }

    /// The inputs `tests/data/statement_splits.json` holds, and how each is cut, or that it is an
    /// error: the table the frontend's splitter is held to as well.
    #[test]
    fn the_shared_table_of_cuts() {
        let cases: Vec<serde_json::Value> =
            serde_json::from_str(include_str!("../../tests/data/statement_splits.json")).unwrap();
        assert!(cases.len() > 30);
        for c in &cases {
            let (why, sql) = (c["why"].as_str().unwrap(), c["sql"].as_str().unwrap());
            match (split_statements(sql), c.get("statements")) {
                (Ok(got), Some(want)) => {
                    let want: Vec<&str> = want
                        .as_array()
                        .unwrap()
                        .iter()
                        .map(|s| s.as_str().unwrap())
                        .collect();
                    assert_eq!(got, want, "{why}: {sql:?}");
                }
                (Err(_), None) if c["error"] == true => {}
                (got, _) => panic!("{why}: {sql:?} gave {got:?}"),
            }
        }
    }

    /// The shape of `examples/*.sql`: a comment above the DDL used to hide `CREATE` from the
    /// classifier, so the schema was counted as a third query and the file was rejected.
    #[test]
    fn leading_comment_does_not_hide_the_keyword() {
        let src = "-- Schema for the pair.\ncreate table \"t\" (\"id\" INTEGER);\n\
                   -- A: one way\nSELECT 1;\n-- B: the other\nSELECT 1;\n";
        let stmts = split_statements(src).unwrap();
        assert_eq!(stmts.len(), 3);
        assert!(stmts[0].to_uppercase().starts_with("CREATE"));
        assert_eq!(stmts[1], "SELECT 1");
        assert_eq!(stmts[2], "SELECT 1");
    }

    #[test]
    fn mask_blanks_literals_and_comments_in_place() {
        let sql = "SELECT 'a$1b', x /* $2 */ FROM t -- $3\nWHERE y = $4";
        let m = mask(sql).unwrap();
        assert_eq!(m.len(), sql.len());
        assert_eq!(m.matches('$').count(), 1, "{m}");
        assert!(m.contains("'    '"), "{m}");
        assert!(m.ends_with("y = $4"));
    }
}
