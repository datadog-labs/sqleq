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
///
/// The tokenizer reports 1-based line and *character* columns; tokens arrive in source order, so
/// one forward walk converts them all.
pub fn lex(sql: &str) -> Option<Vec<Tok>> {
    let tokens = Tokenizer::new(&PostgreSqlDialect {}, sql)
        .tokenize_with_location()
        .ok()?;
    let mut cur = Cursor::new(sql);
    let mut out = Vec::with_capacity(tokens.len());
    for t in tokens {
        if matches!(t.token, Token::EOF) {
            continue;
        }
        let start = cur.seek(t.span.start)?;
        let end = cur.seek(t.span.end)?;
        out.push(Tok {
            token: t.token,
            start,
            end,
            span: t.span,
        });
    }
    Some(out)
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
