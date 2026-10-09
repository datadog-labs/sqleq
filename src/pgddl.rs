// Unless explicitly stated otherwise all files in this repository are licensed under the
// Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/).
// Copyright 2026-Present Datadog, Inc.

//! Build a catalog from the **raw Postgres DDL** a corpus row carries.
//!
//! A row's third field is whatever `pg_dump`-shaped DDL the pair was captured against: real
//! Postgres types, real constraint syntax, and a good deal that the input format's own
//! `CREATE TABLE` subset does not accept. Reading it here, rather than translating it into that
//! subset first, is what lets the frontend lower a row directly — and it keeps the translation
//! from becoming an unreviewed part of the semantics, where `timestamp with time zone` silently
//! becomes a plain `TIMESTAMP`, `numeric` becomes `DOUBLE`, and `integer PRIMARY KEY` becomes
//! `INTEGER` plus a separate `unique (...)`, each read back in as if it had been declared that way.
//!
//! ## One builder, two parsers
//!
//! The tables are built by `catalog::build`, which a pair file's `CREATE TABLE`s go through
//! as well, so a table is named, keyed and cleaned up one way whichever input declared it.
//! What is this module's own is the parse (one statement at a time, with a retry, below) and the
//! type mapping: [`crate::pgddl::map_pg_type`] defers to `map_type_name`, which matches the *base*
//! name against fixed sets and treats anything array/struct/map-shaped, or simply unrecognised, as
//! unmappable, where a pair file keeps an unrecognised type's own name (issue #93).
//!
//! Opaque means `VARBINARY`, which the prover treats as an uninterpreted `Custom` sort supporting `=`
//! only. That is the conservative answer: equality, `IN` and projection keep working, while any
//! ordering or arithmetic use fails loudly instead of quietly assuming an order that a jsonb or a
//! geometry column does not have.


use sqlparser::ast::Statement;
use sqlparser::dialect::PostgreSqlDialect;
use sqlparser::parser::Parser;

use crate::catalog::Catalog;
use crate::infer::{map_type_name, Ty};

/// The opaque type: an uninterpreted sort that supports `=` and nothing else.
pub const OPAQUE: &str = "VARBINARY";

/// Map a rendered Postgres type name to the prover type string a `Catalog` holds, or `None` if it
/// has no faithful mapping.
///
/// `None` is not a failure — it is the honest answer for `jsonb`, `geometry`, `inet`, `bytea`, a
/// float, `uuid`, an enum, or any array. The caller turns it into [`OPAQUE`], under the name
/// `types::opaque_name` gives a float, a `jsonb` or an array of `numeric`, whose `=` is not identity.
///
/// The classification is `map_type_name`'s, not a second copy of it: the rule for reading a
/// Postgres type name is one rule, and this module needing a different *rendering* of the answer is
/// not a reason to fork it. The rendering does differ, deliberately: `Ty::sql` spells `Real` as
/// `DOUBLE` because it writes SQL text for `types::map_type` to read back, whereas a catalog
/// entry goes into the prover's `schemas` verbatim, where the rest of the frontend writes `REAL`. The
/// prover aliases `DOUBLE` onto the same sort, so this is consistency rather than correctness — but a
/// schema disagreeing with the CAST targets in its own queries is a needless thing to leave for
/// someone to debug later.
pub fn map_pg_type(rendered: &str) -> Option<&'static str> {
    // `citext` and `char(n)` keep their own names, as `types::map_type` gives them, so that a query
    // reading such a column is refused (`types::refuse_unfaithful`) rather than handed an opaque
    // value whose `=` is not theirs.
    if let Some(u) = crate::types::unfaithful_type(rendered) {
        return Some(u);
    }
    Some(match map_type_name(rendered).0? {
        Ty::Int => "INTEGER",
        Ty::Real => "REAL",
        Ty::Str => "VARCHAR",
        Ty::Bool => "BOOLEAN",
        // The temporal types keep their own names, as `types::map_type` gives them.
        t @ (Ty::Date | Ty::Time | Ty::Timestamp | Ty::TimestampTz | Ty::Interval) => t.sql(),
        Ty::Opaque => OPAQUE,
    })
}

/// Undo the escaping a corpus row's DDL field carries: an optional pair of wrapping quotes, then
/// `\n`, `\t` and `\"` as two-character sequences.
fn unescape(raw: &str) -> String {
    let s = raw.trim();
    let s = match (s.strip_prefix('"'), s.strip_suffix('"')) {
        (Some(_), Some(_)) if s.len() >= 2 => &s[1..s.len() - 1],
        _ => s,
    };
    s.replace("\\n", "\n").replace("\\t", "\t").replace("\\\"", "\"")
}

/// Raw Postgres DDL as the statements this module reads it as, each as the original text, before
/// any parse. For a caller that must hand the DDL to a real database rather than to our parser.
pub fn split_ddl(raw: &str) -> Vec<String> {
    let sql = unescape(raw);
    split_statements(&sql)
        .into_iter()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect()
}

/// Split SQL text into top-level statements on `;`, ignoring separators inside string literals,
/// quoted identifiers, dollar-quoted bodies and comments.
///
/// Needed because a DDL dump is parsed one statement at a time — see [`parse_provided_schema`] — and
/// a naive split would cut a `DEFAULT 'a;b'` in half.
fn split_statements(sql: &str) -> Vec<&str> {
    let b = sql.as_bytes();
    let (mut out, mut start, mut i) = (Vec::new(), 0usize, 0usize);
    while i < b.len() {
        match b[i] {
            b'\'' | b'"' => {
                let q = b[i];
                i += 1;
                while i < b.len() {
                    if b[i] == q {
                        // A doubled quote is an escaped quote, not the end of the literal.
                        if b.get(i + 1) == Some(&q) {
                            i += 2;
                            continue;
                        }
                        break;
                    }
                    i += 1;
                }
            }
            b'-' if b.get(i + 1) == Some(&b'-') => {
                while i < b.len() && b[i] != b'\n' {
                    i += 1;
                }
            }
            b'/' if b.get(i + 1) == Some(&b'*') => {
                i += 2;
                while i < b.len() && !(b[i] == b'*' && b.get(i + 1) == Some(&b'/')) {
                    i += 1;
                }
                i += 1;
            }
            b'$' => {
                // `$tag$ ... $tag$` — the tag is empty or an identifier.
                let tag_end = b[i + 1..]
                    .iter()
                    .position(|c| !(c.is_ascii_alphanumeric() || *c == b'_'))
                    .map(|p| i + 1 + p);
                match tag_end {
                    Some(e) if b[e] == b'$' => {
                        let tag = &sql[i..=e];
                        match sql[e + 1..].find(tag) {
                            Some(p) => i = e + 1 + p + tag.len() - 1,
                            None => i = b.len(),
                        }
                    }
                    _ => {}
                }
            }
            b';' => {
                out.push(&sql[start..i]);
                start = i + 1;
            }
            _ => {}
        }
        i += 1;
    }
    if !sql[start..].trim().is_empty() {
        out.push(&sql[start..]);
    }
    out
}

/// Span of the outermost parenthesised region — a `CREATE TABLE`'s column list.
fn body_span(stmt: &str) -> Option<(usize, usize)> {
    let b = stmt.as_bytes();
    let (mut i, mut depth, mut open) = (0usize, 0i32, None);
    while i < b.len() {
        match b[i] {
            q @ (b'\'' | b'"') => {
                i += 1;
                while i < b.len() && b[i] != q {
                    i += 1;
                }
            }
            b'(' => {
                if depth == 0 {
                    open = Some(i);
                }
                depth += 1;
            }
            b')' => {
                depth -= 1;
                if depth == 0 {
                    return open.map(|o| (o + 1, i));
                }
            }
            _ => {}
        }
        i += 1;
    }
    None
}

/// Split a column list at its top-level commas, so `numeric(10,2)` stays in one piece.
fn split_items(body: &str) -> Vec<&str> {
    let b = body.as_bytes();
    let (mut out, mut start, mut depth, mut i) = (Vec::new(), 0usize, 0i32, 0usize);
    while i < b.len() {
        match b[i] {
            q @ (b'\'' | b'"') => {
                i += 1;
                while i < b.len() && b[i] != q {
                    i += 1;
                }
            }
            b'(' => depth += 1,
            b')' => depth -= 1,
            b',' if depth == 0 => {
                out.push(&body[start..i]);
                start = i + 1;
            }
            _ => {}
        }
        i += 1;
    }
    if !body[start..].trim().is_empty() {
        out.push(&body[start..]);
    }
    out
}

/// Spans of the bare words in `s`, with the paren depth each sits at. Text inside quotes is skipped,
/// so a word here is always an identifier or a keyword, never part of a literal.
fn word_spans(s: &str) -> Vec<(usize, usize, i32)> {
    let b = s.as_bytes();
    let (mut out, mut i, mut depth) = (Vec::new(), 0usize, 0i32);
    while i < b.len() {
        match b[i] {
            q @ (b'\'' | b'"') => {
                i += 1;
                while i < b.len() && b[i] != q {
                    i += 1;
                }
                i += 1;
            }
            b'(' => {
                depth += 1;
                i += 1;
            }
            b')' => {
                depth -= 1;
                i += 1;
            }
            c if c.is_ascii_alphabetic() || c == b'_' => {
                let st = i;
                while i < b.len() && (b[i].is_ascii_alphanumeric() || b[i] == b'_') {
                    i += 1;
                }
                out.push((st, i, depth));
            }
            _ => i += 1,
        }
    }
    out
}

/// Column options that can follow a `DEFAULT` expression, and so end it.
const AFTER_DEFAULT: &[&str] = &[
    "NOT",
    "PRIMARY",
    "UNIQUE",
    "REFERENCES",
    "CHECK",
    "CONSTRAINT",
    "COLLATE",
    "GENERATED",
    "DEFERRABLE",
];

/// Rewrite one item of a column list into something with the same catalog content but less syntax.
fn simplify_item(item: &str) -> String {
    let ws = word_spans(item);
    let Some(&(s0, e0, _)) = ws.first() else { return item.to_string() };
    let first = item[s0..e0].to_ascii_uppercase();
    let second = ws.get(1).map(|&(s, e, _)| item[s..e].to_ascii_uppercase()).unwrap_or_default();
    // `PRIMARY`/`UNIQUE` lead both a table constraint and a perfectly ordinary column name; what
    // separates them is what follows.
    let constraint = matches!(
        first.as_str(),
        "CONSTRAINT" | "CHECK" | "EXCLUDE" | "FOREIGN" | "LIKE" | "PERIOD"
    ) || (matches!(first.as_str(), "PRIMARY" | "UNIQUE")
        && (second == "KEY" || second == "NULLS" || item[e0..].trim_start().starts_with('(')));

    let mut out = item.to_string();
    // Replace the default expression with a marker. Stopping short leaves text that fails to parse
    // and costs the table, as before; overshooting drops a `NOT NULL`, which only widens the
    // instance space and so can only cost proofs. Neither direction can invent a constraint that is
    // not there.
    //
    // The marker, not deletion: [`crate::catalog::row_determined`] reads the default, and a column
    // whose default we deleted is indistinguishable from one that never had a default -- which is
    // the *unsound* direction, since a stripped `nextval()` would then license the `INSERT`
    // reduction to prove a pair that reorders rows. An unknown nullary call is the honest
    // rendering of "there was a default here and we could not parse it", and it parses.
    if let Some(p) = ws
        .iter()
        .position(|&(s, e, d)| d == 0 && s > s0 && item[s..e].eq_ignore_ascii_case("default"))
    {
        let end = ws[p + 1..]
            .iter()
            .find(|&&(s, e, d)| d == 0 && AFTER_DEFAULT.contains(&item[s..e].to_ascii_uppercase().as_str()))
            .map_or(item.len(), |&(s, _, _)| s);
        out.replace_range(ws[p].0..end, "DEFAULT qed_unparsed_default() ");
    }
    // Quote the column name, folded to lower case first as Postgres folds an unquoted name: quoted,
    // it would otherwise keep its case, and the catalog stores a quoted name as written. Its only
    // effect is to let a column named `primary` or `order` through. A word is ASCII letters, digits
    // and underscores (see [`word_spans`]), so the ASCII fold is the whole of Postgres's.
    if !constraint && !item.trim_start().starts_with('"') {
        out.replace_range(s0..e0, &format!("\"{}\"", item[s0..e0].to_ascii_lowercase()));
    }
    out
}

/// A `CREATE TABLE` that would not parse, with the parts this module never reads simplified away.
///
/// Returns `None` for anything that is not a `CREATE TABLE` with a column list — an index or a view
/// has nothing here to simplify.
fn simplify_for_retry(stmt: &str) -> Option<String> {
    let up = stmt.trim_start().to_ascii_uppercase();
    let head = &up[..up.find('(').unwrap_or(up.len())];
    if !up.starts_with("CREATE") || !head.contains("TABLE") {
        return None;
    }
    let (bs, be) = body_span(stmt)?;
    let items: Vec<String> = split_items(&stmt[bs..be]).iter().map(|i| simplify_item(i)).collect();
    Some(format!("{}{}{}", &stmt[..bs], items.join(","), &stmt[be..]))
}

/// Parse raw Postgres DDL into a catalog.
///
/// Unparseable input yields an empty catalog rather than an error, matching the preprocessor: a row
/// whose DDL we cannot read is a row with no declared schema, which is a state the rest of the
/// pipeline already handles.
///
/// **Statements are parsed one at a time**, and one that fails is skipped rather than abandoning the
/// row. That is not a nicety: these dumps interleave `CREATE TABLE`s with `CREATE INDEX ... INCLUDE
/// (...)` and other Postgres-isms sqlparser does not accept, and parsing the dump as a unit loses
/// every table in it to the first index it cannot read. That is not hypothetical: real dumps do
/// carry such statements, and whole schemas were lost to them before this became statement-at-a-time.
///
/// Statements that are not `CREATE TABLE` — including those standalone `CREATE UNIQUE INDEX`es — are
/// skipped, so the keys they would contribute are not picked up. That is a completeness gap, not a
/// soundness one: a missed key only costs proofs.
pub fn parse_provided_schema(raw: &str) -> Catalog {
    parse_reporting(raw).0
}

/// Raw Postgres DDL, parsed one statement at a time, plus one report per statement that would not
/// parse even after the retry.
///
/// The `bool` is `true` for a statement that only parsed after the retry (`simplify_for_retry`). A
/// caller that reads more than the catalog does must know which those are: the retry replaces every
/// default in the table with `qed_unparsed_default()`, and it quotes every column name the DDL left
/// unquoted, folded to lower case first (`MyCol` comes back as `"mycol"`), so the result no longer
/// says which names the DDL quoted.
pub fn parse_statements_reporting(raw: &str) -> (Vec<(Statement, bool)>, Vec<Rejected>) {
    let sql = unescape(raw);
    let mut errors = Vec::new();
    let statements = split_statements(&sql)
        .into_iter()
        .filter_map(|s| match Parser::parse_sql(&PostgreSqlDialect {}, s) {
            Ok(st) => Some(st.into_iter().map(|st| (st, false)).collect::<Vec<_>>()),
            Err(e) => {
                let retry = simplify_for_retry(s)
                    .and_then(|r| Parser::parse_sql(&PostgreSqlDialect {}, &r).ok());
                if retry.is_none() {
                    errors.push(Rejected { message: e.to_string(), statement: s.trim().to_string() });
                }
                retry.map(|st| st.into_iter().map(|st| (st, true)).collect())
            }
        })
        .flatten()
        .collect();
    (statements, errors)
}

/// One statement this module could not read, and why.
///
/// The statement is carried in full rather than truncated into the message, because the only useful
/// thing to do with a rejection is hand it to a *second* parser and ask whether the DDL is invalid
/// or our reader is too narrow. A truncated prefix is not re-parseable.
#[derive(Clone, Debug)]
pub struct Rejected {
    pub message: String,
    pub statement: String,
}

/// [`parse_provided_schema`], plus one report per statement that would not parse.
///
/// Silently dropping a statement is right for the pipeline — a table we cannot read is a table we
/// have no schema for — but it leaves no way to say *which* construct cost a table. The reports
/// exist so that question stays answerable: a batch harness can route them to a second parser and
/// learn what the first one choked on, and a production caller can log them.
pub fn parse_reporting(raw: &str) -> (Catalog, Vec<Rejected>) {
    let (statements, errors) = parse_statements_reporting(raw);
    let parsed: Vec<&Statement> = statements.iter().map(|(st, _)| st).collect();
    (crate::catalog::build(&parsed, crate::catalog::TypeMap::Raw), errors)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base_names_classify_off_the_head_of_the_type() {
        assert_eq!(map_pg_type("timestamp without time zone"), Some("TIMESTAMP"));
        assert_eq!(map_pg_type("timestamp with time zone"), Some("TIMESTAMPTZ"));
        assert_eq!(map_pg_type("date"), Some("DATE"));
        assert_eq!(map_pg_type("interval"), Some("INTERVAL"));
        assert_eq!(map_pg_type("time with time zone"), None);
        assert_eq!(map_pg_type("numeric(10,2)"), Some("REAL"));
        assert_eq!(map_pg_type("character varying(255)"), Some("VARCHAR"));
        // A float is opaque: it rounds, and the IR's REAL is exact.
        assert_eq!(map_pg_type("double precision"), None);
        assert_eq!(map_pg_type("real"), None);
        assert_eq!(map_pg_type("BIGSERIAL"), Some("INTEGER"));
        assert_eq!(map_pg_type("boolean"), Some("BOOLEAN"));
        // `uuid` reads `'{A0EE…}'` and `'a0ee…'` as one value, so it is not text; as in a declared
        // `CREATE TABLE`, it is opaque.
        assert_eq!(map_pg_type("uuid"), None);
        // Integers by name: these contain `INT` and are not integers.
        assert_eq!(map_pg_type("int4range"), None);
        assert_eq!(map_pg_type("point"), None);
        // `=` ignores case or trailing spaces: kept by name, so a query reading one is refused.
        assert_eq!(map_pg_type("citext"), Some("CITEXT"));
        assert_eq!(map_pg_type("character(3)"), Some("BPCHAR"));
        assert_eq!(map_pg_type("bpchar"), Some("BPCHAR"));
    }

    #[test]
    fn arrays_and_unknown_types_are_opaque_not_guessed() {
        // The bug this rule exists to prevent: a substring classifier reads `TEXT` out of `text[]`.
        assert_eq!(map_pg_type("text[]"), None);
        assert_eq!(map_pg_type("ARRAY<TEXT>"), None);
        assert_eq!(map_pg_type("integer[]"), None);
        assert_eq!(map_pg_type("jsonb"), None);
        assert_eq!(map_pg_type("geometry"), None);
        assert_eq!(map_pg_type("bytea"), None);
        assert_eq!(map_pg_type("inet"), None);
    }

    #[test]
    fn ddl_becomes_a_catalog_with_types_keys_and_nullability() {
        let cat = parse_provided_schema(
            r#"CREATE TABLE public.orders (
                 id integer PRIMARY KEY,
                 total numeric,
                 note text,
                 tags text[],
                 created_at timestamp without time zone NOT NULL
               );"#,
        );
        assert_eq!(cat.tables.len(), 1);
        let t = &cat.tables[0];
        assert_eq!(t.name, "public.orders", "a table is keyed on its name as declared");
        assert_eq!(
            t.cols,
            vec![
                ("id".into(), "INTEGER".into()),
                ("total".into(), "REAL".into()),
                ("note".into(), "VARCHAR".into()),
                // An array of text: opaque, and its `=` is identity.
                ("tags".into(), crate::types::IDENTITY_OPAQUE.into()),
                ("created_at".into(), "TIMESTAMP".into()),
            ]
        );
        assert_eq!(t.keys, vec![vec![0]]);
        // PRIMARY KEY implies NOT NULL; the explicit NOT NULL is kept too. The preprocessor drops
        // both, which is the one deliberate divergence in this port.
        assert_eq!(t.nullable, vec![false, true, true, true, false]);
    }

    #[test]
    fn escaped_corpus_field_is_unwrapped_before_parsing() {
        let cat = parse_provided_schema(r#""CREATE TABLE t (\n    a integer\n);""#);
        assert_eq!(cat.tables.len(), 1);
        assert_eq!(cat.tables[0].cols, vec![("a".into(), "INTEGER".into())]);
    }

    #[test]
    fn unparseable_ddl_is_an_empty_catalog_not_an_error() {
        assert!(parse_provided_schema("this is not ddl at all").tables.is_empty());
    }

    #[test]
    fn one_unparseable_statement_does_not_lose_the_others() {
        // The real shape: a table, then an index sqlparser cannot read. Parsing the dump as a unit
        // would return nothing at all.
        let cat = parse_provided_schema(
            "CREATE TABLE t (a integer, b integer); \
             CREATE INDEX i ON public.t USING btree (a) INCLUDE (b) WHERE (a > 0); \
             CREATE TABLE u (c text);",
        );
        let names: Vec<&str> = cat.tables.iter().map(|t| t.name.as_str()).collect();
        assert_eq!(names, vec!["t", "u"]);
    }

    #[test]
    fn statement_splitting_ignores_semicolons_inside_literals() {
        let cat = parse_provided_schema("CREATE TABLE t (a text DEFAULT 'x;y', b integer);");
        assert_eq!(cat.tables.len(), 1);
        assert_eq!(cat.tables[0].cols.len(), 2);
    }

    #[test]
    fn the_postgres_quoted_char_type_is_opaque() {
        // `"char"` is a real Postgres type, a parser that keeps the quotes must not lose the table
        // over them, and it is neither `char(n)` nor text: one byte, which reads `'ab'` as `'a'`.
        let cat = parse_provided_schema(r#"CREATE TABLE t (a "char" NOT NULL);"#);
        assert_eq!(cat.tables[0].cols, vec![("a".into(), "VARBINARY".into())]);
    }

    #[test]
    fn a_column_named_for_a_keyword_does_not_cost_the_table() {
        // `primary` is reserved, so this DDL is strictly invalid — and it is what real dumps
        // contain. Before the retry, the whole table went missing.
        let cat = parse_provided_schema(
            "CREATE TABLE t (id bigint PRIMARY KEY, primary boolean NOT NULL, note text);",
        );
        assert_eq!(cat.tables.len(), 1);
        assert_eq!(
            cat.tables[0].cols,
            vec![
                ("id".into(), "INTEGER".into()),
                ("primary".into(), "BOOLEAN".into()),
                ("note".into(), "VARCHAR".into()),
            ]
        );
        assert_eq!(cat.tables[0].keys, vec![vec![0]], "the real PRIMARY KEY still reads as one");
        assert_eq!(cat.tables[0].nullable, vec![false, false, true]);
    }

    #[test]
    fn a_default_expression_we_cannot_parse_does_not_cost_the_table() {
        // `VARIADIC` in an argument list is beyond sqlparser. The retry replaces the expression
        // with the marker rather than deleting it — one bit of a DEFAULT does reach the catalog,
        // and a *deleted* default reads back as row-determined, which is the unsound direction.
        // The options *after* it must survive either way.
        let cat = parse_provided_schema(
            "CREATE TABLE t (\
               a uuid NOT NULL DEFAULT md5_concat(g, VARIADIC ARRAY[x, (y)::text]) UNIQUE,\
               b integer DEFAULT 5 NOT NULL,\
               c text);",
        );
        assert_eq!(cat.tables.len(), 1);
        let t = &cat.tables[0];
        assert_eq!(t.cols.iter().map(|(n, _)| n.as_str()).collect::<Vec<_>>(), ["a", "b", "c"]);
        assert_eq!(t.nullable, vec![false, false, true], "NOT NULL after a rewritten DEFAULT");
        assert_eq!(t.keys, vec![vec![0]], "UNIQUE after a rewritten DEFAULT");
        // Note the second `false`: the retry rewrites *every* default in the table, so `b`'s
        // literal `DEFAULT 5` becomes opaque too. That costs refusals on the tables that need the
        // retry at all and cannot license a proof, which is the direction to err in.
        assert_eq!(t.row_determined, vec![false, false, true], "the rewritten DEFAULT is opaque");
    }

    #[test]
    fn the_retry_does_not_turn_a_table_constraint_into_a_column() {
        // Quoting the leading word of every item would make `PRIMARY KEY (a, b)` a column named
        // `primary`, silently losing the key. The unparseable DEFAULT is what forces the retry:
        // sqlparser's PostgreSQL dialect takes `order`, `grant` and `from` as bare column names,
        // so a reserved-looking name does *not* reach this path.
        let cat = parse_provided_schema(
            "CREATE TABLE t (a integer, b integer, \
               c uuid DEFAULT md5_concat(g, VARIADIC ARRAY[x, (y)::text]), \
               PRIMARY KEY (a, b), UNIQUE (b));",
        );
        let t = &cat.tables[0];
        assert_eq!(t.cols.iter().map(|(n, _)| n.as_str()).collect::<Vec<_>>(), ["a", "b", "c"]);
        assert_eq!(t.keys, vec![vec![0, 1], vec![1]]);
        assert_eq!(t.row_determined, vec![true, true, false]);
    }

    /// The claim the retry test's comment rests on, pinned so a sqlparser upgrade that reserves one
    /// of these names fails here rather than silently rerouting a table through the retry.
    #[test]
    fn a_reserved_looking_column_name_parses_on_the_first_attempt() {
        let cat = parse_provided_schema("CREATE TABLE t (order text, grant text, from text);");
        assert_eq!(
            cat.tables[0].cols.iter().map(|(n, _)| n.as_str()).collect::<Vec<_>>(),
            ["order", "grant", "from"]
        );
        assert_eq!(cat.tables[0].row_determined, vec![true, true, true], "no retry, no marker");
    }

    #[test]
    fn a_column_unique_and_a_table_unique_on_it_are_one_key() {
        let cat = parse_provided_schema("CREATE TABLE t (a integer UNIQUE, b integer, UNIQUE (a));");
        assert_eq!(cat.tables[0].keys, vec![vec![0]]);
    }

    /// The bit [`dml::insert_pair`][crate::dml] reads: is the value this column takes when an
    /// `INSERT` omits it a function of the row being written?
    #[test]
    fn a_default_that_is_not_a_function_of_the_row_is_recorded_as_such() {
        let cat = parse_provided_schema(
            "CREATE TABLE t (
               id bigserial,
               sm smallserial,
               seq integer DEFAULT nextval('s'::regclass),
               uid uuid DEFAULT gen_random_uuid(),
               gen integer GENERATED ALWAYS AS IDENTITY,
               n integer DEFAULT 0,
               m integer DEFAULT -1,
               s text DEFAULT 'x'::text,
               b boolean DEFAULT false,
               ts timestamp DEFAULT now(),
               cur timestamp DEFAULT CURRENT_TIMESTAMP,
               plain integer
             );",
        );
        let t = &cat.tables[0];
        assert_eq!(
            t.cols.iter().map(|(n, _)| n.as_str()).collect::<Vec<_>>(),
            ["id", "sm", "seq", "uid", "gen", "n", "m", "s", "b", "ts", "cur", "plain"]
        );
        assert_eq!(
            t.row_determined,
            vec![false, false, false, false, false, true, true, true, true, true, true, true]
        );
    }

    /// The retry replaces a default it could not keep with a marker rather than deleting it.
    /// Deletion is the unsound direction: a column whose default was deleted is indistinguishable
    /// from one that never had a default, and a stripped `nextval()` would then read as
    /// row-determined and license the `INSERT` reduction to prove a pair that reorders rows.
    #[test]
    fn the_retry_marks_a_default_it_cannot_keep_rather_than_dropping_it() {
        assert_eq!(
            simplify_item("a integer DEFAULT nextval('s'::regclass) NOT NULL"),
            r#""a" integer DEFAULT qed_unparsed_default() NOT NULL"#
        );
        // And the marker is not row-determined, so the column is conservatively volatile — which
        // also makes every *ordinary* default on a retried table volatile. A refusal, not a proof.
        let cat = parse_provided_schema(
            "CREATE TABLE t (a integer DEFAULT qed_unparsed_default(), b integer);",
        );
        assert_eq!(cat.tables[0].row_determined, vec![false, true]);
    }
}

