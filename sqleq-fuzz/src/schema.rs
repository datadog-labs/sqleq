// Unless explicitly stated otherwise all files in this repository are licensed under the
// Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/).
// Copyright 2026-Present Datadog, Inc.

//! Parsing table schemas out of the corpus DDL.
//!
//! We need three things per table for sound instance generation: column names, column types (to
//! generate valid literals), and **every uniqueness constraint** (PRIMARY KEY, column/table UNIQUE,
//! and `CREATE UNIQUE INDEX`). Missing a uniqueness constraint is *unsound*: we could then fabricate
//! a two-row instance that no valid database admits, and report a bogus counterexample. So on top of
//! the sqlparser AST pass we keep a regex fallback for `CREATE UNIQUE INDEX` (which strict parsers
//! sometimes reject on trailing options). A partial index (`... WHERE`) is treated as *total*
//! uniqueness — conservative (it only shrinks the valid-instance space, so any counterexample stays
//! genuine).
//!
//! Constraints are read wherever Postgres lets the DDL state them: inline on a column, as a table
//! constraint, from `CREATE UNIQUE INDEX`, and from `ALTER TABLE … ADD PRIMARY KEY | UNIQUE` and
//! `ALTER COLUMN … SET NOT NULL`. A unique index over an *expression* is kept as that expression and
//! materialized as a DuckDB unique index ([`Table::expr_keys`]), and `NULLS NOT DISTINCT` is kept
//! ([`Table::nnd_keys`]). A uniqueness statement that cannot be read at all does not silently drop out:
//! it marks its table [`Table::unreadable`], and a pair over that table gets no verdict.

use std::collections::{BTreeMap, BTreeSet};

use regex::Regex;
use sqlparser::ast::{
    AlterColumnOperation, AlterTableOperation, ColumnDef, ColumnOption, DataType, ExactNumberInfo,
    Expr, GeneratedAs, IndexColumn, NullsDistinctOption, ObjectName, ObjectNamePart, Statement,
    TableConstraint,
};
use sqlparser::dialect::PostgreSqlDialect;
use sqlparser::parser::Parser;

/// A column's value domain, mapped to how it is generated and rendered for DuckDB.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VType {
    Integer,
    /// `real`, `double precision`, `float`: binary floating point in both engines.
    Double,
    /// `numeric`/`decimal`, as DuckDB `DECIMAL(width, scale)`. Postgres `numeric` arithmetic is exact,
    /// and so is DuckDB's DECIMAL wherever it does not raise instead -- which a DOUBLE is not: there
    /// `x + 0.1 + 0.2 <> x + 0.3`. A `numeric(p,s)` keeps its scale, since Postgres rounds a stored
    /// value to it; a bare `numeric` (no typmod) gets [`BARE_NUMERIC`].
    Decimal(u8, u8),
    Boolean,
    Date,
    Timestamp,
    /// `timestamptz` / `timestamp with time zone`, materialized as DuckDB `TIMESTAMPTZ` under the
    /// session time zone `open_db` fixes to UTC. Generated as a naive timestamp it would compute
    /// `ts AT TIME ZONE 'X'` in the opposite direction from Postgres, which converts an instant to a
    /// local time where a naive value is read as a local time.
    TimestampTz,
    Varchar,
    /// Kept distinct from `Varchar` because DuckDB's `IN`/`ANY`/`ALL` refuses to compare a UUID
    /// against a VARCHAR: a `uuid` column generated as VARCHAR cannot be matched against
    /// `= ANY($1::uuid[])` at all, however uuid-shaped the strings in it are.
    Uuid,
    /// Kept distinct from `Varchar` because DuckDB parses the *content* of a JSON value on every
    /// accessor: `j ->> 'k'` over a column materialized as the VARCHAR `'a'` is an
    /// `Invalid Input Error: Malformed JSON at byte 0`, which makes every statement in the pair
    /// unrunnable rather than merely unselective. The domain it generates from is
    /// [`crate::gen::JSONS`].
    Json,
    /// `interval`, as DuckDB `INTERVAL`. Its name contains `INT`, which is how it used to become an
    /// INTEGER filled with `0`, `1` and `2`. DuckDB's interval arithmetic is not Postgres's either
    /// (under `integer_division` it has no `/` at all), so a pair that reads such a column gets no
    /// verdict ([`crate::pair::test_pair`]); the type only has to hold the values of a table the pair
    /// does not read.
    Interval,
}

/// The DuckDB type a bare `numeric` (no typmod) is materialized as, as a column or as a cast target
/// (`rewrite::wide_numerics`). Postgres keeps every digit of such a value; DuckDB needs a fixed scale,
/// and on its own reads a bare `DECIMAL` as `DECIMAL(18,3)`, which rounds `0.0002` to `0.000`. Eighteen
/// fractional digits keep every value this crate generates or a query is likely to compute exact, and
/// leave room for the product of two such values (scale 36): DuckDB raises rather than rounds when a
/// product needs more than 38, which costs a trial and never invents a difference.
pub const BARE_NUMERIC: (u8, u8) = (38, 18);

impl VType {
    /// The DuckDB column type used in generated `CREATE TABLE`s.
    pub fn sql(self) -> String {
        match self {
            VType::Integer => "INTEGER".to_string(),
            VType::Double => "DOUBLE".to_string(),
            VType::Decimal(w, s) => format!("DECIMAL({w},{s})"),
            VType::Boolean => "BOOLEAN".to_string(),
            VType::Date => "DATE".to_string(),
            VType::Timestamp => "TIMESTAMP".to_string(),
            VType::TimestampTz => "TIMESTAMPTZ".to_string(),
            VType::Varchar => "VARCHAR".to_string(),
            VType::Uuid => "UUID".to_string(),
            VType::Json => "JSON".to_string(),
            VType::Interval => "INTERVAL".to_string(),
        }
    }

    /// Whether two domains generate the same values -- so a value drawn from a column of one
    /// satisfies a cast to the other. `numeric` and the floating types share the `{0, 1, 2}` pool, and
    /// a timestamp is generated alike with or without a time zone.
    pub fn same_domain(self, other: VType) -> bool {
        let class = |v: VType| match v {
            VType::Decimal(..) => VType::Double,
            VType::TimestampTz => VType::Timestamp,
            v => v,
        };
        class(self) == class(other)
    }
}

#[derive(Clone, Debug)]
pub struct Column {
    pub name: String,
    /// For an array column this is the *element* type; `array` says whether it is wrapped.
    pub vt: VType,
    pub notnull: bool,
    /// A one-dimensional Postgres array (`text[]`, `uuid[]`, ...), materialized as a DuckDB LIST.
    ///
    /// Generating a declared `text[]` as a scalar VARCHAR is not merely a lost verdict: it makes
    /// every array operator over the column fail to bind, and any counterexample the pair does
    /// produce describes a database the declared schema forbids. Postgres multi-dimensional arrays
    /// are not modelled — the corpus declares none — so this is a flag rather than a rank.
    pub array: bool,
    /// Declared `char(n)` / `character(n)` / `bpchar`: a blank-padded type. Postgres pads the stored
    /// value and ignores trailing blanks when comparing two of them, so `c = 'a'` and `c = 'a  '`
    /// agree, while `c LIKE 'a'` is false. Materialized as a DuckDB VARCHAR it has none of that, and a
    /// pair that reads such a column gets no verdict ([`crate::pair::test_pair`]).
    pub padded: bool,
    /// Filled from a sequence: a `serial` (`serial2/4/8`, `smallserial`, `bigserial`) or an identity
    /// column (`GENERATED … AS IDENTITY`). Both are NOT NULL in Postgres, which [`Column::notnull`]
    /// carries, and a fresh table's sequence never repeats a value, so the generator draws distinct
    /// ones ([`Table::admits`]). Postgres does not enforce that (an explicit value may repeat one),
    /// so it is not a key: no DuckDB constraint is created and no key-based reasoning relies on it.
    pub sequenced: bool,
}

#[derive(Clone, Debug, Default)]
pub struct Table {
    pub cols: Vec<Column>,
    /// Uniqueness constraints as lower-cased column-name lists (PK / UNIQUE / UNIQUE INDEX).
    pub keys: Vec<Vec<String>>,
    /// The keys among [`Table::keys`] declared `NULLS NOT DISTINCT`: at most one row may hold NULL in
    /// such a key, which a DuckDB `UNIQUE` (NULLs distinct) does not enforce. The generator drops a row
    /// that would be a second one ([`Table::admits`]).
    pub nnd_keys: Vec<Vec<String>>,
    /// Unique indexes over expressions (`CREATE UNIQUE INDEX … (lower(c))`), each as the list of its
    /// index expressions rendered for DuckDB. No column list can stand for one, so each is created as
    /// a DuckDB unique index of its own, which rejects a violating row on insert exactly as the
    /// column keys do. A partial index is enforced as a total one, as everywhere in this module.
    pub expr_keys: Vec<Vec<String>>,
    /// Why this table's constraints could not all be read -- a `CREATE UNIQUE INDEX` or `ALTER TABLE`
    /// the parser rejected and no fallback could read. Generating rows for it would ignore a
    /// constraint, so no pair over the table gets a verdict.
    pub unreadable: Option<String>,
}

impl Table {
    /// Whether a row may join `kept` under the `NULLS NOT DISTINCT` keys -- no earlier row holds the
    /// same values there, NULL counting as a value -- and with a value of its own in every
    /// [`Column::sequenced`] column. The other constraints are DuckDB's to enforce.
    pub fn admits<V: PartialEq>(&self, kept: &[Vec<V>], row: &[V]) -> bool {
        let fresh = self.cols.iter().enumerate().all(|(i, c)| {
            !c.sequenced || !kept.iter().any(|other| other[i] == row[i])
        });
        fresh && self.nnd_keys.iter().all(|key| {
            let idx: Vec<usize> = key
                .iter()
                .filter_map(|c| self.cols.iter().position(|col| &col.name == c))
                .collect();
            idx.is_empty()
                || !kept
                    .iter()
                    .any(|other| idx.iter().all(|&i| other[i] == row[i]))
        })
    }
}

/// Table key -> table.
///
/// The key is the table's lower-cased name when the DDL declares one table of that name, whatever its
/// schema -- DDL often omits the schema the queries spell, so every spelling of the name means that
/// table ([`resolve`]). When the DDL declares the same name in several schemas (`s1.t`, `s2.t`), those
/// are different tables, and each is keyed `schema.name` (an unqualified declaration as `public.name`)
/// and draws rows of its own.
pub type Schema = BTreeMap<String, Table>;

/// The schema key a table reference resolves to, from its lower-cased name parts.
///
/// A name declared once resolves from any spelling. A name declared in several schemas resolves only
/// through the schema it names, a bare name meaning `public` (Postgres's default `search_path`), so a
/// reference the DDL does not settle resolves to nothing rather than to a guess.
pub fn resolve<'a>(schema: &'a Schema, parts: &[String]) -> Option<&'a String> {
    let name = parts.last()?;
    if let Some((k, _)) = schema.get_key_value(name) {
        return Some(k);
    }
    let qual = if parts.len() >= 2 {
        parts[parts.len() - 2].as_str()
    } else {
        "public"
    };
    schema
        .get_key_value(&format!("{qual}.{name}"))
        .map(|(k, _)| k)
}

/// Undo the CSV cell escaping used by the corpus (surrounding quotes + `\n`/`\"`/`\'`/`\t`).
pub fn clean_ddl(s: &str) -> String {
    let mut s = s.trim().to_string();
    if s.len() >= 2 && s.starts_with('"') && s.ends_with('"') {
        s = s[1..s.len() - 1].to_string();
    }
    s.replace("\\n", "\n")
        .replace("\\\"", "\"")
        .replace("\\'", "'")
        .replace("\\t", "\t")
}

/// How a declared column type is materialized: its generation domain, whether it is an array, whether
/// it is blank-padded, and -- when DuckDB has no faithful type for it -- why not.
#[derive(Clone, Debug, PartialEq)]
struct ColType {
    vt: VType,
    array: bool,
    padded: bool,
    /// A serial type: NOT NULL, and filled from a sequence ([`Column::sequenced`]).
    serial: bool,
    problem: Option<String>,
}

/// Classify a sqlparser type into a generation domain, plus whether it is an array. The element
/// domain is decided on the rendered type name so it stays robust across type spellings (SERIAL,
/// TIMESTAMPTZ, NUMERIC(p,s), ...); array-ness is read off the AST node instead, which is exact for
/// every spelling the parser recognises rather than only the `[]` suffix.
fn map_vtype(dt: &DataType) -> ColType {
    use sqlparser::ast::ArrayElemTypeDef as A;
    match dt {
        DataType::Array(A::SquareBracket(inner, _))
        | DataType::Array(A::AngleBracket(inner))
        | DataType::Array(A::Parenthesis(inner))
        | DataType::Array(A::Qualified(inner, _)) => ColType {
            array: true,
            ..map_vtype(inner)
        },
        // `ARRAY` with no element type: the domain is unknowable, and VARCHAR is the same default
        // an unrecognised scalar type gets.
        DataType::Array(A::None) => ColType {
            vt: VType::Varchar,
            array: true,
            padded: false,
            serial: false,
            problem: None,
        },
        DataType::Numeric(info) | DataType::Decimal(info) | DataType::Dec(info) => {
            let (p, s) = match info {
                ExactNumberInfo::None => (None, None),
                ExactNumberInfo::Precision(p) => (Some(*p as i64), None),
                ExactNumberInfo::PrecisionAndScale(p, s) => (Some(*p as i64), Some(*s)),
            };
            decimal_col(p, s)
        }
        _ => col_type(&format!("{dt}")),
    }
}

/// A `numeric(p, s)` column as a DuckDB `DECIMAL`, or the reason it has none. DuckDB's DECIMAL stops
/// at 38 digits; a wider `numeric` keeps its scale (the only place the typmod changes a value, by
/// rounding what is stored) and loses integer digits, which only makes DuckDB raise on an overflow
/// Postgres would not have -- a lost trial, not a different answer. A scale DuckDB cannot carry
/// (negative, larger than the precision, or past 37) would round differently, so it is refused.
fn decimal_col(p: Option<i64>, s: Option<i64>) -> ColType {
    let ok = |w: i64, s: i64| ColType {
        vt: VType::Decimal(w as u8, s as u8),
        array: false,
        padded: false,
        serial: false,
        problem: None,
    };
    match (p, s) {
        (None, _) => ok(BARE_NUMERIC.0 as i64, BARE_NUMERIC.1 as i64),
        (Some(p), s) => {
            let s = s.unwrap_or(0);
            if p < 1 || s < 0 || s > p || s > 37 {
                return ColType {
                    problem: Some(format!("numeric({p},{s}) has no DuckDB DECIMAL")),
                    ..ok(BARE_NUMERIC.0 as i64, BARE_NUMERIC.1 as i64)
                };
            }
            ok(p.min(38), s)
        }
    }
}

/// Classify a type by its leading word, upper-cased, and say whether it is an array. Multi-word
/// spellings all decide on that first word (`CHARACTER VARYING` → VARCHAR, `DOUBLE PRECISION` →
/// DOUBLE), which is why the regex fallback can share this with the AST path; `TIMESTAMP WITH TIME
/// ZONE` is the one spelling whose tail decides.
///
/// The `[]` suffix is stripped *before* the leading word is taken, because it is the only place an
/// array declaration differs from its element type: `TEXT[]` would otherwise fall through every
/// element test and land on the `VARCHAR` default, which is exactly the bug this pair of return
/// values exists to prevent. Only the one-dimensional suffix is recognised; the corpus declares no
/// `[][]` and no sized `[N]`, and both would read here as a plain array rather than be guessed at.
fn col_type(raw: &str) -> ColType {
    let trimmed = raw.trim();
    let array = trimmed.ends_with(']');
    let elem = if array {
        trimmed[..trimmed.rfind('[').unwrap_or(trimmed.len())].trim()
    } else {
        trimmed
    };
    // A leading word is all the element tests look at; the `(p,s)` tail is read only for numerics.
    let base: &str = elem
        .split(|c: char| c == '(' || c.is_whitespace())
        .next()
        .unwrap_or(elem);
    let upper = elem.to_uppercase();
    let word = base.trim_matches('"').to_uppercase();
    let padded = matches!(word.as_str(), "CHAR" | "CHARACTER" | "NCHAR" | "BPCHAR")
        && !upper.contains("VARYING");
    if matches!(word.as_str(), "NUMERIC" | "DECIMAL" | "DEC") {
        let args: Vec<i64> = elem
            .find('(')
            .and_then(|o| elem[o + 1..].find(')').map(|c| &elem[o + 1..o + 1 + c]))
            .map(|inner| {
                inner
                    .split(',')
                    .filter_map(|x| x.trim().parse().ok())
                    .collect()
            })
            .unwrap_or_default();
        return ColType {
            array,
            ..decimal_col(args.first().copied(), args.get(1).copied())
        };
    }
    let vt = if word == "TIMESTAMP" && upper.contains("WITH TIME ZONE") {
        VType::TimestampTz
    } else {
        vtype_word(&word)
    };
    // Shorthand for an integer column that is NOT NULL and defaults to `nextval(..)`. An array of
    // one is not a type Postgres has.
    let serial = !array
        && matches!(
            word.as_str(),
            "SERIAL" | "SERIAL2" | "SERIAL4" | "SERIAL8" | "SMALLSERIAL" | "BIGSERIAL"
        );
    ColType {
        vt,
        array,
        padded,
        serial,
        problem: None,
    }
}

/// The element-domain half of [`col_type`], on a single already-isolated leading word.
fn vtype_word(base: &str) -> VType {
    let base = base.to_uppercase();
    if base.contains("BOOL") {
        VType::Boolean
    } else if base == "UUID" {
        VType::Uuid
    } else if base.contains("JSON") {
        // `JSON` and `JSONB` both, and no ordering hazard against the tests below: neither spelling
        // contains INT, SERIAL, or any of the numeric-tower keywords.
        VType::Json
    } else if base == "TIMESTAMPTZ" {
        VType::TimestampTz
    } else if base == "TIMESTAMP" || base == "DATETIME" {
        VType::Timestamp
    } else if base == "DATE" {
        VType::Date
    } else if base == "INTERVAL" {
        // Ahead of the `INT` test below, which its name would otherwise pass.
        VType::Interval
    } else if base.contains("INT") || base.contains("SERIAL") {
        VType::Integer
    } else if ["REAL", "DOUBLE", "FLOAT"].iter().any(|k| base.contains(k)) {
        VType::Double
    } else {
        VType::Varchar
    }
}

/// Split on commas that are outside any parentheses and outside any quoted run.
fn split_top(body: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut depth = 0i32;
    let mut quote: Option<char> = None;
    for ch in body.chars() {
        match quote {
            Some(q) => {
                cur.push(ch);
                if ch == q {
                    quote = None;
                }
            }
            None => match ch {
                '\'' | '"' => {
                    quote = Some(ch);
                    cur.push(ch);
                }
                '(' => {
                    depth += 1;
                    cur.push(ch);
                }
                ')' => {
                    depth -= 1;
                    cur.push(ch);
                }
                ',' if depth == 0 => out.push(std::mem::take(&mut cur)),
                _ => cur.push(ch),
            },
        }
    }
    if !cur.trim().is_empty() {
        out.push(cur);
    }
    out
}

/// Byte index of the `)` matching the `(` at `open`.
fn matching(s: &str, open: usize) -> Option<usize> {
    let mut depth = 0i32;
    let mut quote: Option<char> = None;
    for (i, ch) in s.char_indices().skip_while(|(i, _)| *i < open) {
        match quote {
            Some(q) => {
                if ch == q {
                    quote = None;
                }
            }
            None => match ch {
                '\'' | '"' => quote = Some(ch),
                '(' => depth += 1,
                ')' => {
                    depth -= 1;
                    if depth == 0 {
                        return Some(i);
                    }
                }
                _ => {}
            },
        }
    }
    None
}

/// Take a leading (optionally double-quoted) identifier, returning it lower-cased with the rest.
fn take_ident(s: &str) -> Option<(String, &str)> {
    let s = s.trim_start();
    if let Some(rest) = s.strip_prefix('"') {
        let end = rest.find('"')?;
        let name = rest[..end].to_lowercase();
        if name.is_empty() {
            return None;
        }
        Some((name, &rest[end + 1..]))
    } else {
        let end = s
            .find(|c: char| !(c.is_alphanumeric() || c == '_' || c == '$'))
            .unwrap_or(s.len());
        if end == 0 {
            return None;
        }
        Some((s[..end].to_lowercase(), &s[end..]))
    }
}

/// Whether the type in a column definition carries the one-dimensional `[]` suffix. `s` is the raw
/// text following the column name.
///
/// The suffix has to be found in the raw text and not in [`opts_text`], which strips quotes and
/// parentheses and so reports the `[]` inside `tags text DEFAULT '{}'::text[]` — a *scalar* column.
/// Walking the type region instead — identifier words and `(...)` groups, stopping at the first
/// option keyword — keeps the answer to what the column was actually declared as. `take_ident`
/// cannot help directly: it stops at the `[`, leaving the suffix in the options text.
fn array_suffix(s: &str) -> bool {
    // Every keyword that can open a column option. A type word is never one of these, so reaching
    // one means the type region ended without a `[`.
    const OPTS: [&str; 11] = [
        "not",
        "null",
        "default",
        "primary",
        "unique",
        "check",
        "references",
        "generated",
        "collate",
        "constraint",
        "as",
    ];
    let mut s = s.trim_start();
    loop {
        if let Some(after) = s.strip_prefix('[') {
            // `[]`, `[ ]`, and a sized `[N]` all read as one array dimension.
            let after = after.trim_start();
            return after.starts_with(']')
                || after
                    .trim_start_matches(char::is_numeric)
                    .trim_start()
                    .starts_with(']');
        }
        if s.starts_with('(') {
            let Some(end) = s.find(')') else { return false };
            s = s[end + 1..].trim_start();
            continue;
        }
        let Some((w, next)) = take_ident(s) else {
            return false;
        };
        if OPTS.contains(&w.as_str()) {
            return false;
        }
        s = next.trim_start();
    }
}

/// The type in a column definition, as written up to its first option keyword: the words and the
/// `(…)` groups, without the `[]` suffix (that is [`array_suffix`]'s). `s` is the raw text following
/// the column name. Multi-word types need all their words -- `character varying` is not blank-padded
/// and `timestamp with time zone` is not naive -- and a `numeric` needs its `(p, s)`.
fn type_region(s: &str) -> String {
    const OPTS: [&str; 11] = [
        "not",
        "null",
        "default",
        "primary",
        "unique",
        "check",
        "references",
        "generated",
        "collate",
        "constraint",
        "as",
    ];
    let mut out = String::new();
    let mut s = s.trim_start();
    loop {
        if s.starts_with('(') {
            let Some(end) = s.find(')') else { break };
            out.push_str(&s[..=end]);
            s = s[end + 1..].trim_start();
            continue;
        }
        let Some((w, next)) = take_ident(s) else {
            break;
        };
        if OPTS.contains(&w.as_str()) {
            break;
        }
        if !out.is_empty() {
            out.push(' ');
        }
        out.push_str(&w);
        s = next.trim_start();
    }
    out
}

/// The column list of a table-level `PRIMARY KEY`/`UNIQUE`, or `None` if it is not a plain list of
/// column names (an expression, an operator class, a sort option — anything we would be guessing at).
fn plain_key(s: &str) -> Option<Vec<String>> {
    let open = s.find('(')?;
    let close = matching(s, open)?;
    let mut cols = Vec::new();
    for part in split_top(&s[open + 1..close]) {
        let (name, rest) = take_ident(&part)?;
        if !rest.trim().is_empty() {
            return None;
        }
        cols.push(name);
    }
    (!cols.is_empty()).then_some(cols)
}

/// A column definition's trailing options with string literals and parenthesised groups removed, so
/// `DEFAULT 'not null'` and `REFERENCES t(unique_col)` cannot be mistaken for constraints.
fn opts_text(s: &str) -> String {
    let mut out = String::new();
    let mut depth = 0i32;
    let mut quote: Option<char> = None;
    for ch in s.chars() {
        match quote {
            Some(q) => {
                if ch == q {
                    quote = None;
                }
            }
            None => match ch {
                '\'' | '"' => quote = Some(ch),
                '(' => depth += 1,
                ')' => depth -= 1,
                _ if depth == 0 => out.push(ch.to_ascii_lowercase()),
                _ => {}
            },
        }
    }
    format!(" {} ", out.split_whitespace().collect::<Vec<_>>().join(" "))
}

/// Whether a `CREATE TABLE` body item is a table-level constraint rather than a column definition,
/// and what to do with it. The distinction cannot be made on the leading word alone: the corpus
/// really does declare columns named `primary`, and reading one as a `PRIMARY KEY` would abandon
/// the whole table. So each keyword must be followed by its own syntax.
fn table_constraint(item: &str) -> Option<Kind> {
    if item.starts_with('"') {
        return None;
    }
    let (w, rest) = take_ident(item)?;
    let rest = rest.trim_start();
    let next = take_ident(rest).map(|(w, _)| w).unwrap_or_default();
    match w.as_str() {
        "primary" if next == "key" => Some(Kind::Key),
        "unique" if rest.starts_with('(') || next == "nulls" => Some(Kind::Key),
        "foreign" if next == "key" => Some(Kind::Ignore),
        "check" if rest.starts_with('(') => Some(Kind::Ignore),
        "exclude" if rest.starts_with('(') || next == "using" => Some(Kind::Ignore),
        // `LIKE other` inherits columns we cannot see, so the table is not recoverable.
        "like" => Some(Kind::Abort),
        _ => None,
    }
}

enum Kind {
    /// A uniqueness constraint: must be read, or the table is abandoned.
    Key,
    /// `FOREIGN KEY` / `CHECK` / `EXCLUDE` — ignored, exactly as the AST path ignores them.
    Ignore,
    Abort,
}

/// Read one `CREATE TABLE` body into a table, or `None` if any part of it is unreadable.
///
/// All-or-nothing on purpose. A table recovered with the wrong columns is worse than one that is
/// missing, and — the soundness point — a table recovered *without* one of its uniqueness or NOT
/// NULL constraints would let us generate an instance no valid database admits, which is exactly
/// how a bogus counterexample gets manufactured (see the module docs).
fn recover_body(body: &str) -> Option<Table> {
    let mut t = Table::default();
    let mut pk_cols: Vec<String> = Vec::new();
    for raw in split_top(body) {
        let mut item = raw.trim();
        if item.is_empty() {
            continue;
        }
        // `CONSTRAINT <name> <constraint>` — drop the label, but only when what follows really is a
        // constraint; otherwise this is a column that happens to be named `constraint`.
        if let Some(after) = take_ident(item)
            .filter(|(w, _)| w == "constraint")
            .and_then(|(_, r)| take_ident(r))
            .map(|(_, after)| after.trim())
            .filter(|after| table_constraint(after).is_some())
        {
            item = after;
        }
        match table_constraint(item) {
            Some(Kind::Key) => {
                let key = plain_key(item)?;
                // A table-level PRIMARY KEY makes its columns NOT NULL, exactly as the column-level
                // one does; those columns may be declared after the constraint, so this is applied
                // once the whole body is read.
                if take_ident(item).is_some_and(|(w, _)| w == "primary") {
                    pk_cols.extend(key.iter().cloned());
                }
                if opts_text(item).contains(" nulls not distinct ") {
                    t.nnd_keys.push(key.clone());
                }
                t.keys.push(key);
            }
            Some(Kind::Ignore) => {}
            Some(Kind::Abort) => return None,
            None => {
                let (name, rest) = take_ident(item)?;
                // Read from the raw post-name text, before `opts_text` strips the quotes and
                // parentheses that hide a `[]` belonging to something other than the type.
                let array = array_suffix(rest);
                let ct = col_type(&type_region(rest));
                let (_, opts) = take_ident(rest)?;
                let opts = opts_text(opts);
                // An identity column is NOT NULL, and so is a serial one (`ColType::serial`).
                let sequenced = ct.serial || opts.contains(" as identity ");
                let mut notnull = opts.contains(" not null ") || sequenced;
                if opts.contains(" primary key ") {
                    notnull = true;
                    t.keys.push(vec![name.clone()]);
                } else if opts.contains(" unique ") {
                    t.keys.push(vec![name.clone()]);
                    if opts.contains(" unique nulls not distinct ") {
                        t.nnd_keys.push(vec![name.clone()]);
                    }
                }
                if let Some(why) = ct.problem {
                    t.unreadable.get_or_insert(format!("column {name}: {why}"));
                }
                // Same lower-case collision rule as the AST path: keep the first, take the stronger
                // NOT NULL.
                if let Some(prev) = t.cols.iter_mut().find(|c| c.name == name) {
                    prev.notnull |= notnull;
                } else {
                    t.cols.push(Column {
                        name,
                        vt: ct.vt,
                        notnull,
                        array,
                        padded: ct.padded,
                        sequenced,
                    });
                }
            }
        }
    }
    set_not_null(&mut t, &pk_cols);
    (!t.cols.is_empty()).then_some(t)
}

/// Mark `cols` NOT NULL, as a PRIMARY KEY or `SET NOT NULL` does.
fn set_not_null(t: &mut Table, cols: &[String]) {
    for c in t.cols.iter_mut() {
        if cols.contains(&c.name) {
            c.notnull = true;
        }
    }
}

/// The (lower-cased) column referenced by an index column, if it is a plain identifier.
fn index_col_name(ic: &IndexColumn) -> Option<String> {
    match &ic.column.expr {
        Expr::Identifier(id) => Some(id.value.to_lowercase()),
        Expr::CompoundIdentifier(p) => p.last().map(|id| id.value.to_lowercase()),
        _ => None,
    }
}

/// A uniqueness key as Postgres states it: plain columns when every part is one, else the index
/// expressions rendered for DuckDB (a plain column among them rendered as itself), plus whether
/// NULLs are not distinct.
enum Key {
    Cols(Vec<String>, bool),
    Exprs(Vec<String>, bool),
}

fn key_of(cols: &[IndexColumn], nulls_not_distinct: bool) -> Option<Key> {
    if cols.is_empty() {
        return None;
    }
    let plain: Vec<String> = cols.iter().filter_map(index_col_name).collect();
    if plain.len() == cols.len() {
        return Some(Key::Cols(plain, nulls_not_distinct));
    }
    // Only the expression is kept: an operator class, a collation or a sort order says nothing
    // about which rows collide.
    let exprs = cols.iter().map(|ic| format!("({})", ic.column.expr)).collect();
    Some(Key::Exprs(exprs, nulls_not_distinct))
}

/// Last component of a (possibly qualified) object name, lower-cased.
fn last_name(n: &ObjectName) -> Option<String> {
    n.0.iter().rev().find_map(|p| match p {
        ObjectNamePart::Identifier(id) => Some(id.value.to_lowercase()),
        _ => None,
    })
}

/// The schema a qualified object name names, lower-cased: its next-to-last part.
fn schema_of(n: &ObjectName) -> Option<String> {
    let parts: Vec<String> = n
        .0
        .iter()
        .filter_map(|p| match p {
            ObjectNamePart::Identifier(id) => Some(id.value.to_lowercase()),
            _ => None,
        })
        .collect();
    (parts.len() >= 2).then(|| parts[parts.len() - 2].clone())
}

/// A table as declared: the schema its `CREATE TABLE` names (if any), its name, and what was read.
struct Decl {
    schema: Option<String>,
    name: String,
    table: Table,
}

/// A table a later statement refers to, by (schema, name).
type Target = (Option<String>, String);

/// What a statement other than `CREATE TABLE` adds to a table, applied once every table is known
/// (statement order is free).
enum Pending {
    Key(Target, Key, bool),
    NotNull(Target, Vec<String>),
    AddColumn(Target, Column, Option<String>),
    Unreadable(Target, String),
}

/// One column definition, as the AST states it.
fn column_of(c: &ColumnDef, keys: &mut Vec<(Key, bool)>) -> (Column, Option<String>) {
    let name = c.name.value.to_lowercase();
    let ct = map_vtype(&c.data_type);
    // A serial or identity column is NOT NULL in Postgres whatever else the definition says (an
    // explicit `NULL` on one is an error there), and is filled from a sequence.
    let mut sequenced = ct.serial;
    let mut notnull = ct.serial;
    for opt in &c.options {
        match &opt.option {
            ColumnOption::NotNull => notnull = true,
            // `GENERATED { ALWAYS | BY DEFAULT } AS IDENTITY`; a generated column with an expression
            // (`GENERATED ALWAYS AS (..) STORED`) is neither NOT NULL nor sequenced.
            ColumnOption::Generated {
                generated_as: GeneratedAs::Always | GeneratedAs::ByDefault,
                generation_expr: None,
                ..
            } => {
                notnull = true;
                sequenced = true;
            }
            ColumnOption::PrimaryKey(_) => {
                notnull = true; // PRIMARY KEY implies NOT NULL
                keys.push((Key::Cols(vec![name.clone()], false), true));
            }
            ColumnOption::Unique(u) => keys.push((
                Key::Cols(
                    vec![name.clone()],
                    u.nulls_distinct == NullsDistinctOption::NotDistinct,
                ),
                false,
            )),
            _ => {}
        }
    }
    let problem = ct.problem.map(|why| format!("column {name}: {why}"));
    (
        Column {
            name,
            vt: ct.vt,
            notnull,
            array: ct.array,
            padded: ct.padded,
            sequenced,
        },
        problem,
    )
}

/// Add a key to a table. A primary key also makes its columns NOT NULL.
fn add_key(t: &mut Table, key: Key, primary: bool) {
    match key {
        Key::Cols(cols, nnd) => {
            if primary {
                set_not_null(t, &cols);
            }
            if nnd {
                t.nnd_keys.push(cols.clone());
            }
            t.keys.push(cols);
        }
        Key::Exprs(exprs, nnd) => {
            if nnd {
                // At most one row whose expressions are all NULL: nothing generated can be checked
                // against that without evaluating the expressions, so the table is withheld.
                t.unreadable
                    .get_or_insert("a NULLS NOT DISTINCT unique index on an expression".to_string());
            }
            t.expr_keys.push(exprs);
        }
    }
}

/// The key a table constraint states, if it is a uniqueness constraint, and whether it is primary.
fn constraint_key(con: &TableConstraint) -> Option<(Key, bool)> {
    match con {
        TableConstraint::Unique(uc) => key_of(
            &uc.columns,
            uc.nulls_distinct == NullsDistinctOption::NotDistinct,
        )
        .map(|k| (k, false)),
        TableConstraint::PrimaryKey(pk) => key_of(&pk.columns, false).map(|k| (k, true)),
        _ => None,
    }
}

/// Fold one parsed statement in: a `CREATE TABLE` becomes a declaration; `CREATE UNIQUE INDEX` and
/// `ALTER TABLE` become pending changes to one.
fn absorb(st: &Statement, decls: &mut Vec<Decl>, pending: &mut Vec<Pending>) {
    match st {
        Statement::CreateTable(ct) => {
            let Some(name) = last_name(&ct.name) else {
                return;
            };
            let mut table = Table::default();
            let mut keys: Vec<(Key, bool)> = Vec::new();
            for c in &ct.columns {
                let (col, problem) = column_of(c, &mut keys);
                if let Some(why) = problem {
                    table.unreadable.get_or_insert(why);
                }
                // Postgres folds an unquoted identifier to lower case, so `modelID` and `modelId` are
                // one column and a DDL declaring both is one Postgres rejects outright (`column
                // "modelid" specified more than once`). The table the queries were actually written
                // against therefore had a single `modelid`. DuckDB folds the same way and refuses to
                // create the table at all if both are emitted, so the row loses its verdict to a
                // spelling. Keep the first declaration, but take the *stronger* NOT NULL of the two:
                // treating a NOT NULL column as nullable would widen the instance space, and that is
                // the unsound direction (see the module docs).
                if let Some(prev) = table.cols.iter_mut().find(|c| c.name == col.name) {
                    prev.notnull |= col.notnull;
                } else {
                    table.cols.push(col);
                }
            }
            keys.extend(ct.constraints.iter().filter_map(constraint_key));
            for (key, primary) in keys {
                add_key(&mut table, key, primary);
            }
            decls.push(Decl {
                schema: schema_of(&ct.name),
                name,
                table,
            });
        }
        Statement::CreateIndex(ci) if ci.unique => {
            if let (Some(name), Some(key)) = (
                last_name(&ci.table_name),
                key_of(&ci.columns, ci.nulls_distinct == Some(false)),
            ) {
                pending.push(Pending::Key((schema_of(&ci.table_name), name), key, false));
            }
        }
        Statement::AlterTable(at) => {
            let Some(name) = last_name(&at.name) else {
                return;
            };
            let target: Target = (schema_of(&at.name), name);
            for op in &at.operations {
                match op {
                    AlterTableOperation::AddConstraint { constraint, .. } => {
                        if let Some((key, primary)) = constraint_key(constraint) {
                            pending.push(Pending::Key(target.clone(), key, primary));
                        }
                    }
                    AlterTableOperation::AlterColumn {
                        column_name,
                        op: AlterColumnOperation::SetNotNull,
                    } => pending.push(Pending::NotNull(
                        target.clone(),
                        vec![column_name.value.to_lowercase()],
                    )),
                    AlterTableOperation::AddColumn { column_def, .. } => {
                        let mut keys = Vec::new();
                        let (col, problem) = column_of(column_def, &mut keys);
                        pending.push(Pending::AddColumn(target.clone(), col, problem));
                        for (key, primary) in keys {
                            pending.push(Pending::Key(target.clone(), key, primary));
                        }
                    }
                    // Dropping a constraint or a NOT NULL is ignored, which keeps the instance space
                    // *narrower* than Postgres's: a lost counterexample at worst, never a false one.
                    _ => {}
                }
            }
        }
        _ => {}
    }
}

/// A statement the parser rejected, read for what it would have added to a table: an `ALTER TABLE`
/// or `CREATE UNIQUE INDEX` stating a constraint the fallbacks cannot read marks its table unreadable,
/// since generating rows for the table without it would be unsound. A plain-column unique index is
/// recovered by `IDX_RE` instead, and an `ALTER TABLE` adding a plain-column key or a NOT NULL by
/// the patterns here.
fn absorb_unparsed(chunk: &str, pending: &mut Vec<Pending>) {
    static ALTER: std::sync::LazyLock<Regex> = std::sync::LazyLock::new(|| {
        Regex::new(r#"(?is)^\s*alter\s+table\s+(?:if\s+exists\s+)?(?:only\s+)?((?:"[^"]*"|[A-Za-z_][\w$]*)(?:\s*\.\s*(?:"[^"]*"|[A-Za-z_][\w$]*))*)(.*)$"#).unwrap()
    });
    static ADD_KEY: std::sync::LazyLock<Regex> = std::sync::LazyLock::new(|| {
        Regex::new(r#"(?is)^\s*add\s+(?:constraint\s+(?:"[^"]*"|[A-Za-z_][\w$]*)\s+)?(primary\s+key|unique(?:\s+nulls\s+(?:not\s+)?distinct)?)\s*(\([^()]*\))\s*;?\s*$"#).unwrap()
    });
    static SET_NN: std::sync::LazyLock<Regex> = std::sync::LazyLock::new(|| {
        Regex::new(r#"(?is)^\s*alter\s+(?:column\s+)?("[^"]*"|[A-Za-z_][\w$]*)\s+set\s+not\s+null\s*;?\s*$"#).unwrap()
    });
    static UIDX: std::sync::LazyLock<Regex> = std::sync::LazyLock::new(|| {
        Regex::new(r#"(?is)^\s*create\s+unique\s+index\b.*?\bon\s+(?:only\s+)?((?:"[^"]*"|[A-Za-z_][\w$]*)(?:\s*\.\s*(?:"[^"]*"|[A-Za-z_][\w$]*))*)"#).unwrap()
    });
    let target_of = |raw: &str| -> Target {
        let parts: Vec<String> = raw
            .split('.')
            .map(|p| p.trim().trim_matches('"').to_lowercase())
            .collect();
        let name = parts.last().cloned().unwrap_or_default();
        let schema = (parts.len() >= 2).then(|| parts[parts.len() - 2].clone());
        (schema, name)
    };
    if let Some(caps) = ALTER.captures(chunk) {
        let target = target_of(&caps[1]);
        let rest = &caps[2];
        let states = Regex::new(r"(?i)\b(primary\s+key|unique|not\s+null|exclude)\b")
            .unwrap()
            .is_match(rest);
        if !states {
            return; // renames, defaults, ownership: nothing that bounds the rows
        }
        if let Some(k) = ADD_KEY.captures(rest) {
            if let Some(cols) = plain_key(&k[2]) {
                let primary = k[1].to_lowercase().starts_with("primary");
                let nnd = k[1].to_lowercase().contains("not distinct");
                pending.push(Pending::Key(target, Key::Cols(cols, nnd), primary));
                return;
            }
        }
        if let Some(n) = SET_NN.captures(rest) {
            let col = n[1].trim_matches('"').to_lowercase();
            pending.push(Pending::NotNull(target, vec![col]));
            return;
        }
        pending.push(Pending::Unreadable(
            target,
            "an ALTER TABLE constraint the parser rejected".to_string(),
        ));
    } else if let Some(caps) = UIDX.captures(chunk) {
        // The plain-column form is `IDX_RE`'s; anything else is a key nothing here can read.
        let readable = IDX_RE.captures(chunk).is_some_and(|c| plain_index_cols(&c[2]).is_some())
            && !Regex::new(r"(?i)\bnulls\s+not\s+distinct\b").unwrap().is_match(chunk);
        if !readable {
            pending.push(Pending::Unreadable(
                target_of(&caps[1]),
                "a unique index the parser rejected".to_string(),
            ));
        }
    }
}

/// `CREATE UNIQUE INDEX ... ON t (cols)`, for the statements the parser may have dropped.
static IDX_RE: std::sync::LazyLock<Regex> = std::sync::LazyLock::new(|| {
    Regex::new(r"(?is)create\s+unique\s+index\b.*?\bon\s+([A-Za-z_][\w.]*)[^(]*\(([^)]*)\)")
        .unwrap()
});

/// An index column list that is plain columns only, lower-cased; `None` if any part is not one (a mix
/// means we misread — skip, don't guess).
fn plain_index_cols(raw: &str) -> Option<Vec<String>> {
    static PLAIN_COL: std::sync::LazyLock<Regex> =
        std::sync::LazyLock::new(|| Regex::new(r"^[a-z_]\w*$").unwrap());
    let parts: Vec<&str> = raw.split(',').collect();
    let cols: Vec<String> = parts
        .iter()
        .map(|c| {
            c.chars()
                .filter(|ch| *ch != '"' && !ch.is_whitespace())
                .collect::<String>()
                .to_lowercase()
        })
        .filter(|c| PLAIN_COL.is_match(c))
        .collect();
    (cols.len() == parts.len() && !cols.is_empty()).then_some(cols)
}

/// The declarations a later statement's target refers to: the one table of that name, or -- when
/// several schemas declare it -- the one in the schema it names (a bare name meaning `public`). Where
/// that settles nothing every candidate is returned, so a constraint lands on too many tables rather
/// than on none: a narrower instance space only costs counterexamples.
fn targets(decls: &[Decl], target: &Target) -> Vec<usize> {
    let named: Vec<usize> = (0..decls.len())
        .filter(|&i| decls[i].name == target.1)
        .collect();
    if named.len() <= 1 {
        return named;
    }
    let want = target.0.as_deref().unwrap_or("public");
    let exact: Vec<usize> = named
        .iter()
        .copied()
        .filter(|&i| decls[i].schema.as_deref().unwrap_or("public") == want)
        .collect();
    if exact.is_empty() {
        named
    } else {
        exact
    }
}

/// Parse the DDL for a pair into a schema. Returns an empty map (→ NO-SCHEMA) if nothing usable
/// is found. See the module docs on why uniqueness recovery is soundness-critical.
pub fn parse_schema(ddl: &str) -> Schema {
    parse_schema_stats(ddl).0
}

/// Like [`parse_schema`], also reporting how many uniqueness keys were dropped as unenforceable, so
/// the reach of that rule over a corpus can be measured rather than assumed.
pub fn parse_schema_stats(ddl: &str) -> (Schema, usize) {
    let cleaned = clean_ddl(ddl);
    let dialect = PostgreSqlDialect {};
    let mut decls: Vec<Decl> = Vec::new();
    let mut pending: Vec<Pending> = Vec::new();

    // Prefer one whole-DDL parse; if a single odd statement trips the strict parser, fall back to
    // parsing each `;`-separated statement independently so the good CREATE TABLEs still land.
    match Parser::parse_sql(&dialect, &cleaned) {
        Ok(stmts) => {
            for st in &stmts {
                absorb(st, &mut decls, &mut pending);
            }
        }
        Err(_) => {
            for chunk in cleaned.split(';') {
                if chunk.trim().is_empty() {
                    continue;
                }
                match Parser::parse_sql(&dialect, chunk) {
                    Ok(stmts) => {
                        for st in &stmts {
                            absorb(st, &mut decls, &mut pending);
                        }
                    }
                    Err(_) => absorb_unparsed(chunk, &mut pending),
                }
            }
        }
    }

    // Regex fallback: recover a `CREATE TABLE` the strict parser rejected (an unquoted reserved
    // word for a column name is the corpus's usual reason). Runs before the index passes so a
    // recovered table still collects its uniqueness keys.
    static TBL_RE: std::sync::LazyLock<Regex> = std::sync::LazyLock::new(|| {
        Regex::new(r#"(?is)create\s+(?:global\s+|local\s+|temp\w*\s+|unlogged\s+)*table\s+(?:if\s+not\s+exists\s+)?((?:"[^"]*"|[A-Za-z_][\w$]*)(?:\s*\.\s*(?:"[^"]*"|[A-Za-z_][\w$]*))*)\s*\("#).unwrap()
    });
    for caps in TBL_RE.captures_iter(&cleaned) {
        let parts: Vec<String> = caps[1]
            .split('.')
            .map(|p| p.trim().trim_matches('"').to_lowercase())
            .collect();
        let name = parts.last().cloned().unwrap_or_default();
        let schema = (parts.len() >= 2).then(|| parts[parts.len() - 2].clone());
        if name.is_empty()
            || decls
                .iter()
                .any(|d| d.name == name && d.schema == schema)
        {
            continue;
        }
        let open = caps.get(0).unwrap().end() - 1;
        let Some(close) = matching(&cleaned, open) else {
            continue;
        };
        if let Some(table) = recover_body(&cleaned[open + 1..close]) {
            decls.push(Decl {
                schema,
                name,
                table,
            });
        }
    }

    // Regex fallback: recover `CREATE UNIQUE INDEX ... ON t (cols)` the parser may have dropped.
    // Only plain-column lists are added here; `absorb_unparsed` withholds a table whose unique index
    // it cannot read, and a parsed expression index is kept by `absorb`.
    for caps in IDX_RE.captures_iter(&cleaned) {
        let parts: Vec<String> = caps[1].split('.').map(|p| p.to_lowercase()).collect();
        let name = parts.last().cloned().unwrap_or_default();
        let schema = (parts.len() >= 2).then(|| parts[parts.len() - 2].clone());
        if let Some(cols) = plain_index_cols(&caps[2]) {
            pending.push(Pending::Key((schema, name), Key::Cols(cols, false), false));
        }
    }

    // Apply the constraints other statements state (any statement order).
    for p in pending {
        match p {
            Pending::Key(target, key, primary) => {
                for i in targets(&decls, &target) {
                    let key = match &key {
                        Key::Cols(c, n) => Key::Cols(c.clone(), *n),
                        Key::Exprs(e, n) => Key::Exprs(e.clone(), *n),
                    };
                    add_key(&mut decls[i].table, key, primary);
                }
            }
            Pending::NotNull(target, cols) => {
                for i in targets(&decls, &target) {
                    set_not_null(&mut decls[i].table, &cols);
                }
            }
            Pending::AddColumn(target, col, problem) => {
                for i in targets(&decls, &target) {
                    let t = &mut decls[i].table;
                    if let Some(why) = &problem {
                        t.unreadable.get_or_insert(why.clone());
                    }
                    if !t.cols.iter().any(|c| c.name == col.name) {
                        t.cols.push(col.clone());
                    }
                }
            }
            Pending::Unreadable(target, why) => {
                for i in targets(&decls, &target) {
                    decls[i].table.unreadable.get_or_insert(why.clone());
                }
            }
        }
    }

    // Key the tables: by name, unless the DDL declares that name in more than one schema.
    let mut out = Schema::new();
    for d in &decls {
        let shared = decls
            .iter()
            .filter(|o| o.name == d.name)
            .any(|o| o.schema.as_deref().unwrap_or("public") != d.schema.as_deref().unwrap_or("public"));
        let key = if shared {
            format!("{}.{}", d.schema.as_deref().unwrap_or("public"), d.name)
        } else {
            d.name.clone()
        };
        // A repeated declaration of one table keeps the first, as Postgres would refuse the second.
        out.entry(key).or_insert_with(|| d.table.clone());
    }

    // A key may name a column the table does not declare -- corpus DDL routinely carries
    // `CREATE UNIQUE INDEX ... ON t (c)` for a `c` that the (truncated) `CREATE TABLE t` never
    // lists. Emitting `UNIQUE ("c")` then fails the CREATE TABLE outright and every later
    // reference to the table reports it missing, which costs us the whole pair.
    //
    // Dropping a uniqueness constraint is unsound *in general* -- see the module docs: it lets us
    // fabricate an instance no valid database admits. It is safe here, and only here, because the
    // key names a column we do not model. Take any instance we generate: whether the real column
    // is missing from the DDL or merely missing from our parse, its values are free, so some
    // assignment of distinct values to it satisfies the key. Every instance we can build is
    // therefore the projection of a real one, which is exactly the property a counterexample needs
    // -- unlike dropping a key over columns we *do* generate, which would let two rows collide in
    // a way no real table permits. So the test is exactly "column absent from `cols`": a key whose
    // columns all exist is never touched, however inconvenient it is.
    // A `CREATE UNIQUE INDEX` the parser recognised is also matched by `IDX_RE`, so the same key
    // arrives twice. `ddl_for` already collapses repeated column-sets, so this only removes noise —
    // but it keeps the dropped-key count below meaningful.
    for t in out.values_mut() {
        let mut seen: BTreeSet<Vec<String>> = BTreeSet::new();
        t.keys.retain(|k| seen.insert(k.clone()));
        let mut seen: BTreeSet<Vec<String>> = BTreeSet::new();
        t.nnd_keys.retain(|k| seen.insert(k.clone()));
    }

    let mut dropped = 0usize;
    for t in out.values_mut() {
        let have: BTreeSet<String> = t.cols.iter().map(|c| c.name.clone()).collect();
        let before = t.keys.len();
        t.keys.retain(|k| k.iter().all(|c| have.contains(c)));
        t.nnd_keys.retain(|k| k.iter().all(|c| have.contains(c)));
        dropped += before - t.keys.len();
    }
    (out, dropped)
}
