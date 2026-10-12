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
//!
//! Statements apply in the order they are written, as Postgres runs them (`Reader`): `DROP NOT
//! NULL`, `DROP CONSTRAINT`, `DROP INDEX`, `DROP COLUMN`, `DROP TABLE` and the renames change the
//! tables as they stand where they are written, and a `CREATE TABLE … (LIKE t)` copies `t` as it
//! stands there -- its columns and their NOT NULLs, and its keys with `INCLUDING INDEXES`. A table
//! that `INHERITS` another has the other's columns first; whether a table takes part in inheritance
//! is kept ([`Table::inheritance`]). A drop whose target cannot be told for sure drops nothing,
//! which only narrows the instance space.
//!
//! This is the DuckDB engine's whole picture of the tables. The Postgres engine runs the DDL itself
//! and generates rows for the tables its catalog then holds (`crate::pg`); it reads this model only
//! to find the tables the queries name and to analyse them before the DDL has run.

use std::collections::{BTreeMap, BTreeSet};

use regex::Regex;
use sqlparser::ast::{
    AlterColumnOperation, AlterIndexOperation, AlterTableOperation, ColumnDef, ColumnOption,
    CreateTableLikeKind, DataType, ExactNumberInfo, Expr, GeneratedAs, Ident, IndexColumn,
    NullsDistinctOption, ObjectName, ObjectNamePart, ObjectType, RenameTableNameKind, Statement,
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
    /// `numeric`/`decimal`, as DuckDB `DECIMAL(width, scale)`: a `numeric(p,s)` keeps its precision and
    /// scale, and a bare `numeric` (no typmod) is DuckDB's own bare `DECIMAL`, [`BARE_NUMERIC`].
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
    /// INTEGER filled with `0`, `1` and `2`.
    Interval,
}

/// The DuckDB type a bare `numeric` (no typmod) is materialized as: DuckDB's own reading of a bare
/// `DECIMAL`.
pub const BARE_NUMERIC: (u8, u8) = (18, 3);

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
    /// How the table takes part in inheritance, if it does: `inherits from p`, or `is inherited by
    /// c`. A scan of an inherited table reads its children's rows too, and DuckDB has no
    /// inheritance to build that with, so the DuckDB engine withholds a pair over such a table.
    pub inheritance: Option<String>,
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

/// How a declared column type is materialized: its generation domain, whether it is an array, and --
/// when DuckDB has no type for it -- why not.
#[derive(Clone, Debug, PartialEq)]
struct ColType {
    vt: VType,
    array: bool,
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
        "like" => Some(Kind::Like),
        _ => None,
    }
}

enum Kind {
    /// A uniqueness constraint: must be read, or the table is abandoned.
    Key,
    /// `FOREIGN KEY` / `CHECK` / `EXCLUDE` — ignored, exactly as the AST path ignores them.
    Ignore,
    /// `LIKE other [options]`: the columns of another table, copied where the item stands.
    Like,
}

/// A `CREATE TABLE` body as written: its columns and `LIKE` items in order, and the keys it states.
#[derive(Default)]
struct Body {
    items: Vec<Item>,
    keys: Vec<KeyDecl>,
    /// The names of the body's other constraints (`CHECK`, `FOREIGN KEY`, `EXCLUDE`).
    others: Vec<String>,
    unreadable: Option<String>,
}

/// One column-list item of a `CREATE TABLE`.
enum Item {
    Col(Column),
    /// `LIKE source`, and whether it copies the source's indexes (`INCLUDING INDEXES`, or `ALL`).
    /// Postgres always copies the columns, their types and their `NOT NULL`s.
    Like(Target, bool),
}

/// A leading identifier as Postgres folds it: as written when double-quoted, else lower-cased. What
/// names a constraint or an index, where `"Ix"` and `ix` are two names.
fn take_name(s: &str) -> Option<(String, &str)> {
    let t = s.trim_start();
    match t.strip_prefix('"') {
        Some(rest) => {
            let end = rest.find('"')?;
            let name = &rest[..end];
            (!name.is_empty()).then(|| (name.to_string(), &rest[end + 1..]))
        }
        None => take_ident(t),
    }
}

/// A raw (possibly quoted, possibly qualified) name as a [`Target`]: its last two parts,
/// lower-cased.
fn target_of_raw(raw: &str) -> Target {
    let parts: Vec<String> = raw
        .split('.')
        .map(|p| p.trim().trim_matches('"').to_lowercase())
        .collect();
    let name = parts.last().cloned().unwrap_or_default();
    let schema = (parts.len() >= 2).then(|| parts[parts.len() - 2].clone());
    (schema, name)
}

/// A `LIKE source [{INCLUDING | EXCLUDING} option ...]` body item: the source, and whether its
/// indexes are copied. Options apply left to right, as Postgres applies them (`INCLUDING ALL
/// EXCLUDING INDEXES` copies none). `None` for an option this does not know, which would leave
/// whether a key is copied a guess.
fn like_item(item: &str) -> Option<(Target, bool)> {
    let (_, rest) = take_ident(item)?; // `like`
    let (first, mut rest) = take_ident(rest)?;
    let mut parts = vec![first];
    while let Some(after) = rest.trim_start().strip_prefix('.') {
        let (p, r) = take_ident(after)?;
        parts.push(p);
        rest = r;
    }
    let mut indexes = false;
    loop {
        let r = rest.trim_start();
        if r.is_empty() {
            break;
        }
        let (word, after) = take_ident(r)?;
        let on = match word.as_str() {
            "including" => true,
            "excluding" => false,
            _ => return None,
        };
        let (what, after) = take_ident(after)?;
        match what.as_str() {
            "indexes" | "all" => indexes = on,
            "comments" | "compression" | "constraints" | "defaults" | "generated" | "identity"
            | "statistics" | "storage" => {}
            _ => return None,
        }
        rest = after;
    }
    let name = parts.pop()?;
    Some(((parts.pop(), name), indexes))
}

/// Read one `CREATE TABLE` body, or `None` if any column or key of it is unreadable.
///
/// All-or-nothing on purpose. A table recovered with the wrong columns is worse than one that is
/// missing, and — the soundness point — a table recovered *without* one of its uniqueness or NOT
/// NULL constraints would let us generate an instance no valid database admits, which is exactly
/// how a bogus counterexample gets manufactured (see the module docs). A `LIKE` whose options
/// cannot be read keeps the table, marked unreadable, so that it still shadows any other table of
/// its name.
fn recover_body(body: &str) -> Option<Body> {
    let mut b = Body::default();
    for raw in split_top(body) {
        let mut item = raw.trim();
        if item.is_empty() {
            continue;
        }
        // `CONSTRAINT <name> <constraint>` — drop the label, but only when what follows really is a
        // constraint; otherwise this is a column that happens to be named `constraint`.
        let mut label: Option<String> = None;
        if let Some((name, after)) = take_ident(item)
            .filter(|(w, _)| w == "constraint")
            .and_then(|(_, r)| take_name(r))
            .map(|(name, after)| (name, after.trim()))
            .filter(|(_, after)| table_constraint(after).is_some())
        {
            label = Some(name);
            item = after;
        }
        match table_constraint(item) {
            Some(Kind::Key) => {
                let key = plain_key(item)?;
                // A table-level PRIMARY KEY makes its columns NOT NULL, exactly as the column-level
                // one does; those columns may be declared after the constraint, so that is applied
                // once the whole table is built ([`Reader::add_key`]).
                let primary = take_ident(item).is_some_and(|(w, _)| w == "primary");
                let nnd = opts_text(item).contains(" nulls not distinct ");
                b.keys
                    .push(KeyDecl::new(Key::Cols(key, nnd), primary, false, label));
            }
            Some(Kind::Ignore) => b.others.extend(label),
            Some(Kind::Like) => match like_item(item) {
                Some((src, indexes)) => b.items.push(Item::Like(src, indexes)),
                None => {
                    b.unreadable
                        .get_or_insert("a LIKE whose options cannot be read".to_string());
                }
            },
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
                // A named column constraint keeps its name, which is not read here: such a key is
                // never dropped by name.
                let anon = !opts.contains(" constraint ");
                let key = |nnd: bool, primary: bool| KeyDecl {
                    anon,
                    ..KeyDecl::new(Key::Cols(vec![name.clone()], nnd), primary, false, None)
                };
                if opts.contains(" primary key ") {
                    notnull = true;
                    b.keys.push(key(false, true));
                } else if opts.contains(" unique ") {
                    b.keys
                        .push(key(opts.contains(" unique nulls not distinct "), false));
                }
                if let Some(why) = ct.problem {
                    b.unreadable.get_or_insert(format!("column {name}: {why}"));
                }
                b.items.push(Item::Col(Column {
                    name,
                    vt: ct.vt,
                    notnull,
                    array,
                    sequenced,
                }));
            }
        }
    }
    (!b.items.is_empty() || b.unreadable.is_some()).then_some(b)
}

/// Mark `cols` NOT NULL, as a PRIMARY KEY or `SET NOT NULL` does.
fn set_not_null(t: &mut Table, cols: &[String]) {
    for c in t.cols.iter_mut() {
        if cols.contains(&c.name) {
            c.notnull = true;
        }
    }
}

/// Add a column to a column list, or -- when one of its name is there already -- keep that one and
/// take the *stronger* NOT NULL of the two.
///
/// Postgres folds an unquoted identifier to lower case, so `modelID` and `modelId` are one column
/// and a DDL declaring both is one Postgres rejects outright (`column "modelid" specified more than
/// once`). The table the queries were actually written against therefore had a single `modelid`.
/// DuckDB folds the same way and refuses to create the table at all if both are emitted, so the row
/// loses its verdict to a spelling. Treating a NOT NULL column as nullable would widen the instance
/// space, and that is the unsound direction (see the module docs). An `INHERITS` table merges a
/// column of its own into the parent's of that name the same way.
fn merge_col(cols: &mut Vec<Column>, col: Column) {
    if let Some(prev) = cols.iter_mut().find(|c| c.name == col.name) {
        prev.notnull |= col.notnull;
    } else {
        cols.push(col);
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
#[derive(Clone)]
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

/// A key as declared: what it binds, and the name a later `DROP CONSTRAINT` or `DROP INDEX` drops
/// it by.
#[derive(Clone)]
struct KeyDecl {
    key: Key,
    primary: bool,
    /// A unique index (`CREATE UNIQUE INDEX`), which `DROP INDEX` drops, rather than a constraint,
    /// which `ALTER TABLE … DROP CONSTRAINT` drops.
    index: bool,
    /// Its name, as the DDL gives it or as Postgres generated it; `None` when neither is known for
    /// sure, and then nothing drops it.
    name: Option<String>,
    /// Unnamed in the DDL, so Postgres generates its name ([`generated_name`]).
    anon: bool,
    /// `name` is the generated one. Postgres keeps a generated name unique by appending a number,
    /// so it is the key's only while no other object has it.
    generated: bool,
}

impl KeyDecl {
    fn new(key: Key, primary: bool, index: bool, name: Option<String>) -> KeyDecl {
        KeyDecl {
            key,
            primary,
            index,
            anon: name.is_none(),
            name,
            generated: false,
        }
    }

    fn cols(&self) -> Option<&[String]> {
        match &self.key {
            Key::Cols(c, _) => Some(c),
            Key::Exprs(..) => None,
        }
    }

    /// Whether an expression of the key may read `col`: a textual test, so it says yes too often.
    fn may_read(&self, col: &str) -> bool {
        match &self.key {
            Key::Exprs(e, _) => e.iter().any(|x| x.to_lowercase().contains(col)),
            Key::Cols(..) => false,
        }
    }
}

/// The name Postgres generates for an unnamed key of table `table`: `t_pkey` for a primary key,
/// `t_a_b_key` for a unique constraint, `t_a_b_idx` for a unique index. `None` for an index over an
/// expression, and for a name past Postgres's 63 bytes, which it truncates.
fn generated_name(table: &str, kd: &KeyDecl) -> Option<String> {
    let name = if kd.primary && !kd.index {
        format!("{table}_pkey")
    } else {
        let suffix = if kd.index { "idx" } else { "key" };
        format!("{table}_{}_{suffix}", kd.cols()?.join("_"))
    };
    (name.len() <= 63).then_some(name)
}

/// An identifier as Postgres folds it: as written when double-quoted, else lower-cased.
fn fold(id: &Ident) -> String {
    match id.quote_style {
        Some(_) => id.value.clone(),
        None => id.value.to_lowercase(),
    }
}

/// The parts of an object name, each folded as Postgres folds it.
fn folded_parts(n: &ObjectName) -> Vec<String> {
    n.0.iter()
        .filter_map(|p| match p {
            ObjectNamePart::Identifier(id) => Some(fold(id)),
            _ => None,
        })
        .collect()
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

fn target_of(n: &ObjectName) -> Option<Target> {
    Some((schema_of(n), last_name(n)?))
}

/// A table as declared so far: the schema its `CREATE TABLE` names (if any), its name, its columns,
/// and the keys it has now.
struct Decl {
    schema: Option<String>,
    name: String,
    /// The columns, and why the table cannot be read, if so; the keys are `keys`, until
    /// [`Reader::finish`].
    table: Table,
    keys: Vec<KeyDecl>,
}

/// A table a later statement refers to, by (schema, name).
type Target = (Option<String>, String);

/// A statement that only adds to a table, held when no table of the name it gives is declared yet
/// -- captured DDL is not always in the order it ran in -- and applied once every statement is
/// read, as a narrower instance space only costs counterexamples.
enum Pending {
    Key(Target, KeyDecl),
    NotNull(Target, Vec<String>),
    AddColumn(Target, Column, Option<String>),
    Unreadable(Target, String),
}

impl Pending {
    fn target(&self) -> &Target {
        match self {
            Pending::Key(t, _)
            | Pending::NotNull(t, _)
            | Pending::AddColumn(t, ..)
            | Pending::Unreadable(t, _) => t,
        }
    }
}

/// One column definition, as the AST states it; its keys go to `keys`.
fn column_of(
    c: &ColumnDef,
    keys: &mut Vec<KeyDecl>,
    others: &mut Vec<String>,
) -> (Column, Option<String>) {
    let name = c.name.value.to_lowercase();
    let ct = map_vtype(&c.data_type);
    // A serial or identity column is NOT NULL in Postgres whatever else the definition says (an
    // explicit `NULL` on one is an error there), and is filled from a sequence.
    let mut sequenced = ct.serial;
    let mut notnull = ct.serial;
    for opt in &c.options {
        let label = opt.name.as_ref().map(fold);
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
                keys.push(KeyDecl::new(Key::Cols(vec![name.clone()], false), true, false, label));
            }
            ColumnOption::Unique(u) => keys.push(KeyDecl::new(
                Key::Cols(
                    vec![name.clone()],
                    u.nulls_distinct == NullsDistinctOption::NotDistinct,
                ),
                false,
                false,
                label,
            )),
            _ => others.extend(label),
        }
    }
    let problem = ct.problem.map(|why| format!("column {name}: {why}"));
    (
        Column {
            name,
            vt: ct.vt,
            notnull,
            array: ct.array,
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

/// The name a table constraint is given, folded.
fn constraint_name(con: &TableConstraint) -> Option<String> {
    let id = match con {
        TableConstraint::Unique(c) => c.name.as_ref(),
        TableConstraint::PrimaryKey(c) => c.name.as_ref(),
        TableConstraint::Check(c) => c.name.as_ref(),
        TableConstraint::ForeignKey(c) => c.name.as_ref(),
        TableConstraint::Exclude(c) => c.name.as_ref(),
        TableConstraint::PrimaryKeyUsingIndex(c) | TableConstraint::UniqueUsingIndex(c) => {
            c.name.as_ref()
        }
        _ => None,
    };
    id.map(fold)
}

/// A table constraint of a `CREATE TABLE` or an `ALTER TABLE … ADD`: its key to `keys`, or, when it
/// states none, its name to `others`.
fn table_constraint_of(con: &TableConstraint, keys: &mut Vec<KeyDecl>, others: &mut Vec<String>) {
    match constraint_key(con) {
        Some((key, primary)) => keys.push(KeyDecl::new(key, primary, false, constraint_name(con))),
        None => others.extend(constraint_name(con)),
    }
}

/// The declarations a later statement's target refers to: the one table of that name, or -- when
/// several schemas declare it -- the one in the schema it names (a bare name meaning `public`).
/// Where that settles nothing every candidate is returned, so a constraint lands on too many tables
/// rather than on none: a narrower instance space only costs counterexamples.
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

/// The DDL read statement by statement, in the order it is written, as Postgres runs it: each
/// `ALTER TABLE`, `DROP` and `RENAME` changes the tables as they stand at that point, and a `LIKE`
/// copies its source as it stands there.
///
/// A statement that only *adds* to a table -- a key, a NOT NULL, a column -- and names a table not
/// yet declared is held until every statement is read ([`Pending`]): captured DDL is not always in
/// the order it ran in, and a constraint applied late only narrows the instance space. A statement
/// that *removes* or *renames* something is never guessed at: when no table of its name is declared
/// it does nothing (Postgres would refuse it, or `IF EXISTS` skip it), and when its name could be
/// either of two tables, both are withheld.
#[derive(Default)]
struct Reader {
    decls: Vec<Decl>,
    pending: Vec<Pending>,
    /// Every (schema, name) a statement created, dropped or renamed away: the regex pass over the
    /// raw text recovers none of these again.
    seen: BTreeSet<Target>,
    /// (table, columns) of every plain-column unique index a statement was read for, so the regex
    /// pass over the raw text does not add it again -- after a `DROP INDEX`, say.
    indexed: BTreeSet<(String, Vec<String>)>,
    /// The names of constraints and indexes that are not keys: a generated name that one of these
    /// has already is not the key's.
    others: BTreeSet<String>,
}

impl Reader {
    fn find(&self, t: &Target) -> Vec<usize> {
        targets(&self.decls, t)
    }

    /// Apply a statement that only adds to a table to every table it may name, or hold it.
    fn add(&mut self, p: Pending) {
        let found = self.find(p.target());
        if found.is_empty() {
            self.pending.push(p);
            return;
        }
        for i in found {
            self.apply(i, &p);
        }
    }

    fn apply(&mut self, i: usize, p: &Pending) {
        match p {
            Pending::Key(_, kd) => self.add_key(i, kd.clone()),
            Pending::NotNull(_, cols) => set_not_null(&mut self.decls[i].table, cols),
            Pending::AddColumn(_, col, problem) => {
                let t = &mut self.decls[i].table;
                if let Some(why) = problem {
                    t.unreadable.get_or_insert(why.clone());
                }
                if !t.cols.iter().any(|c| c.name == col.name) {
                    t.cols.push(col.clone());
                }
            }
            Pending::Unreadable(_, why) => {
                self.decls[i].table.unreadable.get_or_insert(why.clone());
            }
        }
    }

    /// The one table a statement that removes or renames something names: none when no table of the
    /// name is declared, and none -- each candidate withheld -- when the name could be either of
    /// two.
    fn one(&mut self, t: &Target, what: &str) -> Option<usize> {
        let found = self.find(t);
        match found.len() {
            0 => None,
            1 => Some(found[0]),
            _ => {
                for i in found {
                    self.decls[i].table.unreadable.get_or_insert(format!(
                        "{what} of a table the DDL declares in several schemas"
                    ));
                }
                None
            }
        }
    }

    /// How many objects have `name`: keys of every table, and the other constraints and indexes.
    fn holders(&self, name: &str) -> usize {
        self.decls
            .iter()
            .flat_map(|d| &d.keys)
            .filter(|k| k.name.as_deref() == Some(name))
            .count()
            + usize::from(self.others.contains(name))
    }

    /// Add a key to table `i`, under the name Postgres generates for it when the DDL gives none --
    /// if no other object has that name, which would make Postgres choose another.
    fn add_key(&mut self, i: usize, mut kd: KeyDecl) {
        if kd.name.is_none() && kd.anon {
            if let Some(n) = generated_name(&self.decls[i].name, &kd) {
                if self.holders(&n) == 0 {
                    kd.name = Some(n);
                    kd.generated = true;
                }
            }
        }
        let d = &mut self.decls[i];
        if kd.primary {
            if let Key::Cols(cols, _) = &kd.key {
                set_not_null(&mut d.table, cols);
            }
        }
        d.keys.push(kd);
    }

    /// Declare a table. An `INHERITS` table has its parents' columns first, then its own; a `LIKE`
    /// item copies its source's columns where it stands, and its keys with `INCLUDING INDEXES`.
    fn create(&mut self, schema: Option<String>, name: String, body: Body, parents: Vec<Target>) {
        // A second declaration of one table is one Postgres refuses (or `IF NOT EXISTS` skips).
        if self.decls.iter().any(|d| d.name == name && d.schema == schema) {
            return;
        }
        self.seen.insert((schema.clone(), name.clone()));
        self.others.extend(body.others);
        let mut table = Table {
            unreadable: body.unreadable,
            ..Table::default()
        };
        let mut copied: Vec<KeyDecl> = Vec::new();
        for p in &parents {
            match self.find(p).as_slice() {
                [j] => {
                    let parent = &mut self.decls[*j].table;
                    for c in parent.cols.clone() {
                        merge_col(&mut table.cols, c);
                    }
                    if let Some(why) = &parent.unreadable {
                        table
                            .unreadable
                            .get_or_insert(format!("inherits from {}: {why}", p.1));
                    }
                    parent
                        .inheritance
                        .get_or_insert(format!("is inherited by {name}"));
                    table
                        .inheritance
                        .get_or_insert(format!("inherits from {}", p.1));
                }
                _ => {
                    table.unreadable.get_or_insert(format!(
                        "inherits from {}, which the DDL does not declare once",
                        p.1
                    ));
                    table
                        .inheritance
                        .get_or_insert(format!("inherits from {}", p.1));
                }
            }
        }
        for item in body.items {
            match item {
                Item::Col(c) => merge_col(&mut table.cols, c),
                Item::Like(src, indexes) => match self.find(&src).as_slice() {
                    [j] => {
                        let s = &self.decls[*j];
                        for c in s.table.cols.clone() {
                            merge_col(&mut table.cols, c);
                        }
                        if let Some(why) = &s.table.unreadable {
                            table
                                .unreadable
                                .get_or_insert(format!("LIKE {}: {why}", src.1));
                        }
                        if indexes {
                            // Each copy is named after the new table, as Postgres names it.
                            copied.extend(s.keys.iter().map(|k| KeyDecl {
                                name: None,
                                anon: true,
                                generated: false,
                                ..k.clone()
                            }));
                        }
                    }
                    _ => {
                        table.unreadable.get_or_insert(format!(
                            "LIKE {}, which the DDL does not declare once",
                            src.1
                        ));
                    }
                },
            }
        }
        self.decls.push(Decl {
            schema,
            name,
            table,
            keys: Vec::new(),
        });
        let i = self.decls.len() - 1;
        for kd in copied.into_iter().chain(body.keys) {
            self.add_key(i, kd);
        }
    }

    fn drop_table(&mut self, t: &Target) {
        if let Some(i) = self.one(t, "DROP TABLE") {
            let d = self.decls.remove(i);
            self.seen.insert((d.schema, d.name));
        }
    }

    fn rename_table(&mut self, t: &Target, new: String) {
        if let Some(i) = self.one(t, "RENAME") {
            let d = &mut self.decls[i];
            self.seen.insert((d.schema.clone(), d.name.clone()));
            d.name = new;
            self.seen.insert((d.schema.clone(), d.name.clone()));
        }
    }

    /// `DROP NOT NULL`, which Postgres refuses on a primary key's column.
    fn drop_not_null(&mut self, t: &Target, col: &str) {
        let Some(i) = self.one(t, "DROP NOT NULL") else {
            return;
        };
        let d = &mut self.decls[i];
        let in_primary = d
            .keys
            .iter()
            .any(|k| k.primary && k.cols().is_some_and(|c| c.iter().any(|x| x == col)));
        if !in_primary {
            if let Some(c) = d.table.cols.iter_mut().find(|c| c.name == col) {
                c.notnull = false;
            }
        }
    }

    /// `DROP COLUMN`, which drops every key over the column with it.
    fn drop_columns(&mut self, t: &Target, cols: &[String]) {
        let Some(i) = self.one(t, "DROP COLUMN") else {
            return;
        };
        let d = &mut self.decls[i];
        for col in cols {
            d.table.cols.retain(|c| &c.name != col);
            d.keys
                .retain(|k| k.cols().is_none_or(|c| !c.contains(col)));
            if d.keys.iter().any(|k| k.may_read(col)) {
                d.table.unreadable.get_or_insert(format!(
                    "a unique index on an expression over dropped column {col}"
                ));
            }
        }
    }

    fn rename_column(&mut self, t: &Target, old: &str, new: &str) {
        let Some(i) = self.one(t, "RENAME COLUMN") else {
            return;
        };
        let d = &mut self.decls[i];
        let Some(c) = d.table.cols.iter().position(|c| c.name == old) else {
            d.table
                .unreadable
                .get_or_insert(format!("RENAME COLUMN of {old}, which the DDL does not declare"));
            return;
        };
        d.table.cols[c].name = new.to_string();
        if d.keys.iter().any(|k| k.may_read(old)) {
            d.table.unreadable.get_or_insert(format!(
                "a unique index on an expression over renamed column {old}"
            ));
        }
        for k in d.keys.iter_mut() {
            if let Key::Cols(cs, _) = &mut k.key {
                for x in cs.iter_mut().filter(|x| *x == old) {
                    *x = new.to_string();
                }
            }
        }
    }

    fn retype_column(&mut self, t: &Target, col: &str, dt: &DataType) {
        let Some(i) = self.one(t, "ALTER COLUMN TYPE") else {
            return;
        };
        let ct = map_vtype(dt);
        let d = &mut self.decls[i];
        if let Some(c) = d.table.cols.iter_mut().find(|c| c.name == col) {
            c.vt = ct.vt;
            c.array = ct.array;
        }
        if let Some(why) = ct.problem {
            d.table.unreadable.get_or_insert(format!("column {col}: {why}"));
        }
    }

    /// The keys of table `i` a constraint name names, and whether dropping or renaming them is
    /// sure: a name the DDL gave is, and a generated one is when no other object has it.
    fn named_constraints(&self, i: usize, name: &str) -> (Vec<usize>, bool) {
        let found: Vec<usize> = (0..self.decls[i].keys.len())
            .filter(|&k| {
                let kd = &self.decls[i].keys[k];
                !kd.index && kd.name.as_deref() == Some(name)
            })
            .collect();
        let generated = found.iter().any(|&k| self.decls[i].keys[k].generated);
        let sure = !generated || self.holders(name) == 1;
        (found, sure)
    }

    /// `DROP CONSTRAINT`. A name read as no key's -- a `CHECK`, a foreign key, a key named some way
    /// this does not work out -- drops nothing here, which only narrows the instance space.
    fn drop_constraint(&mut self, t: &Target, name: &str) {
        let Some(i) = self.one(t, "DROP CONSTRAINT") else {
            return;
        };
        let (found, sure) = self.named_constraints(i, name);
        if sure {
            for k in found.into_iter().rev() {
                self.decls[i].keys.remove(k);
            }
        }
    }

    fn rename_constraint(&mut self, t: &Target, old: &str, new: &str) {
        self.others.insert(new.to_string());
        let Some(i) = self.one(t, "RENAME CONSTRAINT") else {
            return;
        };
        let (found, sure) = self.named_constraints(i, old);
        for k in found {
            let kd = &mut self.decls[i].keys[k];
            kd.name = sure.then(|| new.to_string());
            kd.generated = false;
        }
    }

    /// The unique indexes an index name may name: of a table in the schema it gives, or of any
    /// table when it gives none -- its schema is then whichever the search path finds first.
    fn indexes_named(&self, parts: &[String]) -> Vec<(usize, usize)> {
        let Some(name) = parts.last() else {
            return Vec::new();
        };
        let schema = (parts.len() >= 2).then(|| parts[parts.len() - 2].to_lowercase());
        let mut out = Vec::new();
        for (i, d) in self.decls.iter().enumerate() {
            if schema
                .as_deref()
                .is_some_and(|s| d.schema.as_deref().unwrap_or("public") != s)
            {
                continue;
            }
            for (k, kd) in d.keys.iter().enumerate() {
                if kd.index && kd.name.as_deref() == Some(name.as_str()) {
                    out.push((i, k));
                }
            }
        }
        out
    }

    /// Whether `parts` names exactly one unique index for sure: one candidate, and no other index
    /// (a non-unique one) of that name anywhere.
    fn sure_index(&self, parts: &[String], found: &[(usize, usize)]) -> bool {
        found.len() == 1 && parts.last().is_some_and(|n| !self.others.contains(n))
    }

    /// `DROP INDEX`: drops a key only when exactly one unique index may have the name.
    fn drop_index(&mut self, parts: &[String]) {
        let found = self.indexes_named(parts);
        if self.sure_index(parts, &found) {
            let (i, k) = found[0];
            self.decls[i].keys.remove(k);
        }
    }

    /// `ALTER INDEX … RENAME TO`: renames the one unique index the name names. Of several, it leaves
    /// none with a name a later `DROP INDEX` could match; of none, the index renamed is one that is
    /// no key, and its new name is another's.
    fn rename_index(&mut self, parts: &[String], new: String) {
        let found = self.indexes_named(parts);
        let sure = self.sure_index(parts, &found);
        if !sure {
            self.others.insert(new.clone());
        }
        for (i, k) in found {
            let kd = &mut self.decls[i].keys[k];
            kd.name = sure.then(|| new.clone());
            kd.generated = false;
        }
    }

    /// `ADD [CONSTRAINT c] {PRIMARY KEY | UNIQUE} USING INDEX i`: the index becomes the constraint,
    /// and a primary key's columns become NOT NULL. An index this cannot find leaves a primary
    /// key's NOT NULLs unread, so the table is withheld.
    fn promote(&mut self, t: &Target, index: &str, name: Option<String>, primary: bool) {
        let found = self.find(t);
        if found.len() != 1 {
            if primary {
                for i in found {
                    self.decls[i].table.unreadable.get_or_insert(
                        "a PRIMARY KEY USING INDEX on a table declared in several schemas"
                            .to_string(),
                    );
                }
            }
            return;
        }
        let i = found[0];
        let hits: Vec<usize> = (0..self.decls[i].keys.len())
            .filter(|&k| {
                let kd = &self.decls[i].keys[k];
                kd.index && kd.name.as_deref() == Some(index)
            })
            .collect();
        let sure = hits.len() == 1
            && (!self.decls[i].keys[hits[0]].generated || self.holders(index) == 1);
        if !sure {
            if primary {
                self.decls[i].table.unreadable.get_or_insert(format!(
                    "PRIMARY KEY USING INDEX {index}, an index the DDL does not declare once"
                ));
            }
            return;
        }
        let d = &mut self.decls[i];
        let kd = &mut d.keys[hits[0]];
        kd.index = false;
        kd.primary |= primary;
        kd.name = Some(name.unwrap_or_else(|| index.to_string()));
        kd.generated = false;
        if primary {
            if let Key::Cols(cols, _) = kd.key.clone() {
                set_not_null(&mut d.table, &cols);
            }
        }
    }

    /// Fold one parsed statement in.
    fn statement(&mut self, st: &Statement) {
        match st {
            Statement::CreateTable(ct) => {
                let Some(name) = last_name(&ct.name) else {
                    return;
                };
                let mut body = Body::default();
                for c in &ct.columns {
                    // An unquoted `like` is a keyword Postgres reserves, so `(a int, LIKE o)` is a
                    // `LIKE` item, which the parser reads as a column `like` of type `o`.
                    if c.name.quote_style.is_none() && c.name.value.eq_ignore_ascii_case("like") {
                        match (&c.data_type, c.options.is_empty()) {
                            (DataType::Custom(src, mods), true) if mods.is_empty() => {
                                if let Some(src) = target_of(src) {
                                    body.items.push(Item::Like(src, false));
                                }
                            }
                            _ => {
                                let why = "a LIKE whose options cannot be read";
                                body.unreadable.get_or_insert(why.to_string());
                            }
                        }
                        continue;
                    }
                    let (col, problem) = column_of(c, &mut body.keys, &mut body.others);
                    if let Some(why) = problem {
                        body.unreadable.get_or_insert(why);
                    }
                    body.items.push(Item::Col(col));
                }
                // The parser reads `(LIKE t)` with no option but `{INCLUDING | EXCLUDING}
                // DEFAULTS`, so no index is copied; any other option sends the statement to
                // `unparsed`.
                if let Some(CreateTableLikeKind::Parenthesized(l) | CreateTableLikeKind::Plain(l)) =
                    &ct.like
                {
                    if let Some(src) = target_of(&l.name) {
                        body.items.push(Item::Like(src, false));
                    }
                }
                for con in &ct.constraints {
                    table_constraint_of(con, &mut body.keys, &mut body.others);
                }
                let parents = ct.inherits.iter().flatten().filter_map(target_of).collect();
                self.create(schema_of(&ct.name), name, body, parents);
            }
            Statement::CreateIndex(ci) => {
                let name = ci
                    .name
                    .as_ref()
                    .and_then(|n| folded_parts(n).last().cloned());
                if !ci.unique {
                    self.others.extend(name);
                    return;
                }
                if let (Some(table), Some(key)) = (
                    last_name(&ci.table_name),
                    key_of(&ci.columns, ci.nulls_distinct == Some(false)),
                ) {
                    if let Key::Cols(cols, _) = &key {
                        self.indexed.insert((table.clone(), cols.clone()));
                    }
                    self.add(Pending::Key(
                        (schema_of(&ci.table_name), table),
                        KeyDecl::new(key, false, true, name),
                    ));
                }
            }
            Statement::AlterTable(at) => {
                let Some(target) = target_of(&at.name) else {
                    return;
                };
                for op in &at.operations {
                    self.alter(&target, op);
                }
            }
            Statement::AlterIndex {
                name,
                operation: AlterIndexOperation::RenameIndex { index_name },
            } => {
                if let Some(new) = folded_parts(index_name).last().cloned() {
                    self.rename_index(&folded_parts(name), new);
                }
            }
            Statement::Drop {
                object_type, names, ..
            } => match object_type {
                ObjectType::Table => {
                    for t in names.iter().filter_map(target_of) {
                        self.drop_table(&t);
                    }
                }
                ObjectType::Index => {
                    for n in names {
                        self.drop_index(&folded_parts(n));
                    }
                }
                _ => {}
            },
            _ => {}
        }
    }

    /// One `ALTER TABLE` operation.
    fn alter(&mut self, target: &Target, op: &AlterTableOperation) {
        match op {
            AlterTableOperation::AddConstraint { constraint, .. } => match constraint {
                TableConstraint::PrimaryKeyUsingIndex(u) | TableConstraint::UniqueUsingIndex(u) => {
                    let primary = matches!(constraint, TableConstraint::PrimaryKeyUsingIndex(_));
                    self.promote(target, &fold(&u.index_name), u.name.as_ref().map(fold), primary);
                }
                con => {
                    let (mut keys, mut others) = (Vec::new(), Vec::new());
                    table_constraint_of(con, &mut keys, &mut others);
                    self.others.extend(others);
                    for kd in keys {
                        self.add(Pending::Key(target.clone(), kd));
                    }
                }
            },
            AlterTableOperation::AlterColumn { column_name, op } => {
                let col = column_name.value.to_lowercase();
                match op {
                    AlterColumnOperation::SetNotNull => {
                        self.add(Pending::NotNull(target.clone(), vec![col]))
                    }
                    AlterColumnOperation::DropNotNull => self.drop_not_null(target, &col),
                    AlterColumnOperation::SetDataType { data_type, .. } => {
                        self.retype_column(target, &col, data_type)
                    }
                    // A default bounds no row a statement may write; `ADD GENERATED … AS IDENTITY`
                    // needs a column that is NOT NULL already.
                    _ => {}
                }
            }
            AlterTableOperation::AddColumn { column_def, .. } => {
                let (mut keys, mut others) = (Vec::new(), Vec::new());
                let (col, problem) = column_of(column_def, &mut keys, &mut others);
                self.others.extend(others);
                self.add(Pending::AddColumn(target.clone(), col, problem));
                for kd in keys {
                    self.add(Pending::Key(target.clone(), kd));
                }
            }
            AlterTableOperation::DropColumn { column_names, .. } => {
                let cols: Vec<String> =
                    column_names.iter().map(|c| c.value.to_lowercase()).collect();
                self.drop_columns(target, &cols);
            }
            AlterTableOperation::RenameColumn {
                old_column_name,
                new_column_name,
            } => self.rename_column(
                target,
                &old_column_name.value.to_lowercase(),
                &new_column_name.value.to_lowercase(),
            ),
            AlterTableOperation::RenameTable {
                table_name: RenameTableNameKind::To(n) | RenameTableNameKind::As(n),
            } => {
                if let Some(new) = last_name(n) {
                    self.rename_table(target, new);
                }
            }
            AlterTableOperation::DropConstraint { name, .. } => {
                self.drop_constraint(target, &fold(name))
            }
            AlterTableOperation::RenameConstraint { old_name, new_name } => {
                self.rename_constraint(target, &fold(old_name), &fold(new_name))
            }
            // Ownership, storage, triggers, row security: nothing that bounds the rows.
            _ => {}
        }
    }

    /// A statement the parser rejected, read for what it does to a table, in its place. A `CREATE
    /// TABLE` is recovered by [`recover_body`]; an `ALTER TABLE` that renames, adds a plain-column
    /// key or sets or drops a NOT NULL by the patterns here; a plain-column unique index by
    /// `IDX_RE`. Any other `ALTER TABLE` or `CREATE UNIQUE INDEX` stating a constraint marks its
    /// table unreadable, since generating rows for the table without it would be unsound.
    fn unparsed(&mut self, chunk: &str) {
        static ALTER: std::sync::LazyLock<Regex> = std::sync::LazyLock::new(|| {
            Regex::new(r#"(?is)^\s*alter\s+table\s+(?:if\s+exists\s+)?(?:only\s+)?((?:"[^"]*"|[A-Za-z_][\w$]*)(?:\s*\.\s*(?:"[^"]*"|[A-Za-z_][\w$]*))*)(.*)$"#).unwrap()
        });
        static ADD_KEY: std::sync::LazyLock<Regex> = std::sync::LazyLock::new(|| {
            Regex::new(r#"(?is)^\s*add\s+(?:constraint\s+("[^"]*"|[A-Za-z_][\w$]*)\s+)?(primary\s+key|unique(?:\s+nulls\s+(?:not\s+)?distinct)?)\s*(\([^()]*\))\s*;?\s*$"#).unwrap()
        });
        static SET_NN: std::sync::LazyLock<Regex> = std::sync::LazyLock::new(|| {
            Regex::new(r#"(?is)^\s*alter\s+(?:column\s+)?("[^"]*"|[A-Za-z_][\w$]*)\s+(set|drop)\s+not\s+null\s*;?\s*$"#).unwrap()
        });
        static RENAME: std::sync::LazyLock<Regex> = std::sync::LazyLock::new(|| {
            Regex::new(r#"(?is)^\s*rename\s+(?:(column\s+)?("[^"]*"|[A-Za-z_][\w$]*)\s+)?to\s+("[^"]*"|[A-Za-z_][\w$]*)\s*;?\s*$"#).unwrap()
        });
        static UIDX: std::sync::LazyLock<Regex> = std::sync::LazyLock::new(|| {
            Regex::new(r#"(?is)^\s*create\s+unique\s+index\b.*?\bon\s+(?:only\s+)?((?:"[^"]*"|[A-Za-z_][\w$]*)(?:\s*\.\s*(?:"[^"]*"|[A-Za-z_][\w$]*))*)"#).unwrap()
        });
        let ident = |raw: &str| raw.trim_matches('"').to_lowercase();
        if let Some(caps) = TBL_RE.captures(chunk) {
            if chunk[..caps.get(0).unwrap().start()].trim().is_empty() {
                self.recover(chunk, &caps);
                return;
            }
        }
        if let Some(caps) = ALTER.captures(chunk) {
            let target = target_of_raw(&caps[1]);
            let rest = &caps[2];
            if let Some(r) = RENAME.captures(rest) {
                // `RENAME [COLUMN] a TO b` or `RENAME TO t`; `RENAME CONSTRAINT` matches neither,
                // and a constraint renamed unread is only one a later DROP CONSTRAINT does not find.
                match r.get(2) {
                    Some(old)
                        if r.get(1).is_some()
                            || !old.as_str().eq_ignore_ascii_case("constraint") =>
                    {
                        self.rename_column(&target, &ident(old.as_str()), &ident(&r[3]))
                    }
                    Some(_) => {}
                    None => self.rename_table(&target, ident(&r[3])),
                }
                return;
            }
            if Regex::new(r"(?is)^\s*rename\b").unwrap().is_match(rest) {
                let constraint = Regex::new(r"(?is)^\s*rename\s+constraint\b").unwrap();
                if !constraint.is_match(rest) {
                    if let Some(i) = self.one(&target, "RENAME") {
                        self.decls[i]
                            .table
                            .unreadable
                            .get_or_insert("a RENAME the parser rejected".to_string());
                    }
                }
                return;
            }
            if let Some(n) = SET_NN.captures(rest) {
                let col = ident(&n[1]);
                if n[2].eq_ignore_ascii_case("set") {
                    self.add(Pending::NotNull(target, vec![col]));
                } else {
                    self.drop_not_null(&target, &col);
                }
                return;
            }
            let states = Regex::new(r"(?i)\b(primary\s+key|unique|not\s+null|exclude)\b")
                .unwrap()
                .is_match(rest);
            if !states {
                // Defaults, ownership, a dropped constraint: nothing that widens the rows unread.
                return;
            }
            if let Some(k) = ADD_KEY.captures(rest) {
                if let Some(cols) = plain_key(&k[3]) {
                    let kind = k[2].to_lowercase();
                    let name = k.get(1).map(|m| {
                        let raw = m.as_str();
                        match raw.strip_prefix('"') {
                            Some(q) => q.trim_end_matches('"').to_string(),
                            None => raw.to_lowercase(),
                        }
                    });
                    let key = Key::Cols(cols, kind.contains("not distinct"));
                    let kd = KeyDecl::new(key, kind.starts_with("primary"), false, name);
                    self.add(Pending::Key(target, kd));
                    return;
                }
            }
            self.add(Pending::Unreadable(
                target,
                "an ALTER TABLE constraint the parser rejected".to_string(),
            ));
        } else if let Some(caps) = UIDX.captures(chunk) {
            let plain = IDX_RE
                .captures(chunk)
                .and_then(|c| plain_index_cols(&c[2]).map(|cols| (target_of_raw(&c[1]), cols)));
            let nnd = Regex::new(r"(?i)\bnulls\s+not\s+distinct\b").unwrap().is_match(chunk);
            match plain {
                Some((target, cols)) if !nnd => {
                    self.indexed.insert((target.1.clone(), cols.clone()));
                    // Its name, if it has one, is not read here: such a key is never dropped by
                    // name.
                    let kd = KeyDecl {
                        anon: false,
                        ..KeyDecl::new(Key::Cols(cols, false), false, true, None)
                    };
                    self.add(Pending::Key(target, kd));
                }
                _ => self.add(Pending::Unreadable(
                    target_of_raw(&caps[1]),
                    "a unique index the parser rejected".to_string(),
                )),
            }
        }
    }

    /// Recover the `CREATE TABLE` `caps` matched in `text`, unless a statement already declared,
    /// dropped or renamed a table of that name.
    fn recover(&mut self, text: &str, caps: &regex::Captures) {
        static INHERITS: std::sync::LazyLock<Regex> = std::sync::LazyLock::new(|| {
            Regex::new(r#"(?is)^\s*inherits\s*\(([^()]*)\)"#).unwrap()
        });
        let (schema, name) = target_of_raw(&caps[1]);
        if name.is_empty() || self.seen.contains(&(schema.clone(), name.clone())) {
            return;
        }
        let open = caps.get(0).unwrap().end() - 1;
        let Some(close) = matching(text, open) else {
            return;
        };
        let Some(body) = recover_body(&text[open + 1..close]) else {
            return;
        };
        let parents: Vec<Target> = INHERITS
            .captures(&text[close + 1..])
            .map(|c| c[1].split(',').map(target_of_raw).filter(|t| !t.1.is_empty()).collect())
            .unwrap_or_default();
        self.create(schema, name, body, parents);
    }

    /// The tables as read: each declaration's keys folded into its [`Table`], keyed as [`Schema`]
    /// says, with the keys over columns it does not have dropped.
    fn finish(self) -> (Schema, usize) {
        let decls = self.decls;
        // Key the tables: by name, unless the DDL declares that name in more than one schema.
        let mut out = Schema::new();
        for d in &decls {
            let shared = decls
                .iter()
                .filter(|o| o.name == d.name)
                .any(|o| {
                    o.schema.as_deref().unwrap_or("public") != d.schema.as_deref().unwrap_or("public")
                });
            let key = if shared {
                format!("{}.{}", d.schema.as_deref().unwrap_or("public"), d.name)
            } else {
                d.name.clone()
            };
            let mut t = d.table.clone();
            // A primary key made its columns NOT NULL when it was added, which dropping it later
            // does not undo; nothing is set here.
            for kd in &d.keys {
                add_key(&mut t, kd.key.clone(), false);
            }
            out.entry(key).or_insert(t);
        }

        // A key may name a column the table does not declare -- corpus DDL routinely carries
        // `CREATE UNIQUE INDEX ... ON t (c)` for a `c` that the (truncated) `CREATE TABLE t` never
        // lists. Emitting `UNIQUE ("c")` then fails the CREATE TABLE outright and every later
        // reference to the table reports it missing, which costs us the whole pair.
        //
        // Dropping a uniqueness constraint is unsound *in general* -- see the module docs: it lets
        // us fabricate an instance no valid database admits. It is safe here, and only here,
        // because the key names a column we do not model. Take any instance we generate: whether
        // the real column is missing from the DDL or merely missing from our parse, its values are
        // free, so some assignment of distinct values to it satisfies the key. Every instance we
        // can build is therefore the projection of a real one, which is exactly the property a
        // counterexample needs -- unlike dropping a key over columns we *do* generate, which would
        // let two rows collide in a way no real table permits. So the test is exactly "column
        // absent from `cols`": a key whose columns all exist is never touched, however
        // inconvenient it is.
        //
        // The same key can arrive twice (two statements may state it). `ddl_for` already collapses
        // repeated column-sets, so this only removes noise — but it keeps the dropped-key count
        // below meaningful.
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
}

/// `CREATE TABLE name (`, for the statements the parser rejected.
static TBL_RE: std::sync::LazyLock<Regex> = std::sync::LazyLock::new(|| {
    Regex::new(r#"(?is)create\s+(?:global\s+|local\s+|temp\w*\s+|unlogged\s+)*table\s+(?:if\s+not\s+exists\s+)?((?:"[^"]*"|[A-Za-z_][\w$]*)(?:\s*\.\s*(?:"[^"]*"|[A-Za-z_][\w$]*))*)\s*\("#).unwrap()
});

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
    let mut r = Reader::default();

    // Prefer one whole-DDL parse; if a single odd statement trips the strict parser, parse each
    // statement independently -- split as Postgres's lexer splits them, so a `;` in a string or a
    // dollar-quoted body ends none -- so the good ones still land. Either way they apply in order.
    match Parser::parse_sql(&dialect, &cleaned) {
        Ok(stmts) => {
            for st in &stmts {
                r.statement(st);
            }
        }
        Err(_) => {
            for chunk in crate::lex::split_statements(&cleaned) {
                match Parser::parse_sql(&dialect, &chunk) {
                    Ok(stmts) => {
                        for st in &stmts {
                            r.statement(st);
                        }
                    }
                    Err(_) => r.unparsed(&chunk),
                }
            }
        }
    }

    // Regex fallback over the raw text, for what no statement above declared: a `CREATE TABLE` (an
    // unquoted reserved word for a column name is the corpus's usual reason the parser rejects one)
    // and a plain-column `CREATE UNIQUE INDEX`, wherever the text holds one -- a function body's
    // included. Only ever adds tables and keys, after every statement has applied.
    for caps in TBL_RE.captures_iter(&cleaned) {
        r.recover(&cleaned, &caps);
    }
    for caps in IDX_RE.captures_iter(&cleaned) {
        let parts: Vec<String> = caps[1].split('.').map(|p| p.to_lowercase()).collect();
        let name = parts.last().cloned().unwrap_or_default();
        let schema = (parts.len() >= 2).then(|| parts[parts.len() - 2].clone());
        if let Some(cols) = plain_index_cols(&caps[2]) {
            if r.indexed.contains(&(name.clone(), cols.clone())) {
                continue;
            }
            let kd = KeyDecl {
                anon: false,
                ..KeyDecl::new(Key::Cols(cols, false), false, true, None)
            };
            r.pending.push(Pending::Key((schema, name), kd));
        }
    }

    // The additions held for a table not declared at their statement.
    for p in std::mem::take(&mut r.pending) {
        for i in r.find(p.target()) {
            r.apply(i, &p);
        }
    }
    r.finish()
}
