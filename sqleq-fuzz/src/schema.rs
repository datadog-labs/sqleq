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

use std::collections::{BTreeMap, BTreeSet};

use regex::Regex;
use sqlparser::ast::{
    ColumnOption, Expr, IndexColumn, ObjectName, ObjectNamePart, Statement, TableConstraint,
};
use sqlparser::dialect::PostgreSqlDialect;
use sqlparser::parser::Parser;

/// A column's value domain, mapped to how it is generated and rendered for DuckDB.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VType {
    Integer,
    Double,
    Boolean,
    Date,
    Timestamp,
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
}

impl VType {
    /// The DuckDB column type used in generated `CREATE TABLE`s.
    pub fn sql(self) -> &'static str {
        match self {
            VType::Integer => "INTEGER",
            VType::Double => "DOUBLE",
            VType::Boolean => "BOOLEAN",
            VType::Date => "DATE",
            VType::Timestamp => "TIMESTAMP",
            VType::Varchar => "VARCHAR",
            VType::Uuid => "UUID",
            VType::Json => "JSON",
        }
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
}

#[derive(Clone, Debug, Default)]
pub struct Table {
    pub cols: Vec<Column>,
    /// Uniqueness constraints as lower-cased column-name lists (PK / UNIQUE / UNIQUE INDEX).
    pub keys: Vec<Vec<String>>,
}

/// Table name (lower-cased) -> table.
pub type Schema = BTreeMap<String, Table>;

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

/// Classify a sqlparser type into a generation domain, plus whether it is an array. The element
/// domain is decided on the rendered type name so it stays robust across type spellings (SERIAL,
/// TIMESTAMPTZ, NUMERIC(p,s), ...); array-ness is read off the AST node instead, which is exact for
/// every spelling the parser recognises rather than only the `[]` suffix.
fn map_vtype(dt: &sqlparser::ast::DataType) -> (VType, bool) {
    use sqlparser::ast::{ArrayElemTypeDef as A, DataType};
    match dt {
        DataType::Array(A::SquareBracket(inner, _))
        | DataType::Array(A::AngleBracket(inner))
        | DataType::Array(A::Parenthesis(inner)) => (map_vtype(inner).0, true),
        // `ARRAY` with no element type: the domain is unknowable, and VARCHAR is the same default
        // an unrecognised scalar type gets.
        DataType::Array(A::None) => (VType::Varchar, true),
        _ => vtype_of(&format!("{dt}")),
    }
}

/// Classify a type by its leading word, upper-cased, and say whether it is an array. Multi-word
/// spellings all decide on that first word (`CHARACTER VARYING` → VARCHAR, `DOUBLE PRECISION` →
/// DOUBLE, `TIMESTAMP WITH TIME ZONE` → TIMESTAMP), which is why the regex fallback can share this
/// with the AST path.
///
/// The `[]` suffix is stripped *before* the leading word is taken, because it is the only place an
/// array declaration differs from its element type: `TEXT[]` would otherwise fall through every
/// element test and land on the `VARCHAR` default, which is exactly the bug this pair of return
/// values exists to prevent. Only the one-dimensional suffix is recognised; the corpus declares no
/// `[][]` and no sized `[N]`, and both would read here as a plain array rather than be guessed at.
fn vtype_of(raw: &str) -> (VType, bool) {
    let trimmed = raw.trim();
    let array = trimmed.ends_with(']');
    let elem = if array {
        trimmed[..trimmed.rfind('[').unwrap_or(trimmed.len())].trim()
    } else {
        trimmed
    };
    // A leading word is all the element tests look at; the `(p,s)` / `WITH TIME ZONE` tail is noise.
    let base: &str = elem
        .split(|c: char| c == '(' || c.is_whitespace())
        .next()
        .unwrap_or(elem);
    (vtype_word(base), array)
}

/// The element-domain half of [`vtype_of`], on a single already-isolated leading word.
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
    } else if base == "TIMESTAMP" || base == "TIMESTAMPTZ" || base == "DATETIME" {
        VType::Timestamp
    } else if base == "DATE" {
        VType::Date
    } else if base.contains("INT") || base.contains("SERIAL") {
        VType::Integer
    } else if ["NUMERIC", "DECIMAL", "REAL", "DOUBLE", "FLOAT"]
        .iter()
        .any(|k| base.contains(k))
    {
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
            Some(Kind::Key) => t.keys.push(plain_key(item)?),
            Some(Kind::Ignore) => {}
            Some(Kind::Abort) => return None,
            None => {
                let (name, rest) = take_ident(item)?;
                // Read from the raw post-name text, before `opts_text` strips the quotes and
                // parentheses that hide a `[]` belonging to something other than the type.
                let array = array_suffix(rest);
                let (ty, opts) = take_ident(rest)?;
                let opts = opts_text(opts);
                let mut notnull = opts.contains(" not null ");
                if opts.contains(" primary key ") {
                    notnull = true;
                    t.keys.push(vec![name.clone()]);
                } else if opts.contains(" unique ") {
                    t.keys.push(vec![name.clone()]);
                }
                // Same lower-case collision rule as the AST path: keep the first, take the stronger
                // NOT NULL.
                if let Some(prev) = t.cols.iter_mut().find(|c| c.name == name) {
                    prev.notnull |= notnull;
                } else {
                    t.cols.push(Column {
                        name,
                        vt: vtype_of(&ty).0,
                        notnull,
                        array,
                    });
                }
            }
        }
    }
    (!t.cols.is_empty()).then_some(t)
}

/// The (lower-cased) column referenced by an index column, if it is a plain identifier.
fn index_col_name(ic: &IndexColumn) -> Option<String> {
    match &ic.column.expr {
        Expr::Identifier(id) => Some(id.value.to_lowercase()),
        Expr::CompoundIdentifier(p) => p.last().map(|id| id.value.to_lowercase()),
        _ => None,
    }
}

/// Last component of a (possibly qualified) object name, lower-cased.
fn last_name(n: &ObjectName) -> Option<String> {
    n.0.iter().rev().find_map(|p| match p {
        ObjectNamePart::Identifier(id) => Some(id.value.to_lowercase()),
        _ => None,
    })
}

/// Fold one parsed statement into the schema (CREATE TABLE columns/keys, CREATE UNIQUE INDEX keys).
/// Index statements are buffered and applied after all tables are known (statement order is free).
fn absorb(st: &Statement, out: &mut Schema, indexes: &mut Vec<(String, Vec<String>)>) {
    match st {
        Statement::CreateTable(ct) => {
            let Some(tname) = last_name(&ct.name) else {
                return;
            };
            let mut table = Table::default();
            for c in &ct.columns {
                let name = c.name.value.to_lowercase();
                let (vt, array) = map_vtype(&c.data_type);
                let mut notnull = false;
                for opt in &c.options {
                    match &opt.option {
                        ColumnOption::NotNull => notnull = true,
                        ColumnOption::PrimaryKey(_) => {
                            notnull = true; // PRIMARY KEY implies NOT NULL
                            table.keys.push(vec![name.clone()]);
                        }
                        ColumnOption::Unique(_) => table.keys.push(vec![name.clone()]),
                        _ => {}
                    }
                }
                // Postgres folds an unquoted identifier to lower case, so `modelID` and `modelId` are
                // one column and a DDL declaring both is one Postgres rejects outright (`column
                // "modelid" specified more than once`). The table the queries were actually written
                // against therefore had a single `modelid`. DuckDB folds the same way and refuses to
                // create the table at all if both are emitted, so the row loses its verdict to a
                // spelling. Keep the first declaration, but take the *stronger* NOT NULL of the two:
                // treating a NOT NULL column as nullable would widen the instance space, and that is
                // the unsound direction (see the module docs).
                if let Some(prev) = table.cols.iter_mut().find(|c| c.name == name) {
                    prev.notnull |= notnull;
                } else {
                    table.cols.push(Column {
                        name,
                        vt,
                        notnull,
                        array,
                    });
                }
            }
            for con in &ct.constraints {
                let cols: &[IndexColumn] = match con {
                    TableConstraint::Unique(uc) => &uc.columns,
                    TableConstraint::PrimaryKey(pk) => &pk.columns,
                    _ => continue,
                };
                let key: Vec<String> = cols.iter().filter_map(index_col_name).collect();
                if !key.is_empty() {
                    table.keys.push(key);
                }
            }
            out.insert(tname, table);
        }
        Statement::CreateIndex(ci) if ci.unique => {
            if let Some(tn) = last_name(&ci.table_name) {
                let key: Vec<String> = ci.columns.iter().filter_map(index_col_name).collect();
                if !key.is_empty() {
                    indexes.push((tn, key));
                }
            }
        }
        _ => {}
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
    let mut out = Schema::new();
    let mut indexes: Vec<(String, Vec<String>)> = Vec::new();

    // Prefer one whole-DDL parse; if a single odd statement trips the strict parser, fall back to
    // parsing each `;`-separated statement independently so the good CREATE TABLEs still land.
    match Parser::parse_sql(&dialect, &cleaned) {
        Ok(stmts) => {
            for st in &stmts {
                absorb(st, &mut out, &mut indexes);
            }
        }
        Err(_) => {
            for chunk in cleaned.split(';') {
                if chunk.trim().is_empty() {
                    continue;
                }
                if let Ok(stmts) = Parser::parse_sql(&dialect, chunk) {
                    for st in &stmts {
                        absorb(st, &mut out, &mut indexes);
                    }
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
        let raw = &caps[1];
        let tname = raw
            .rsplit('.')
            .next()
            .unwrap_or(raw)
            .trim()
            .trim_matches('"')
            .to_lowercase();
        if tname.is_empty() || out.contains_key(&tname) {
            continue;
        }
        let open = caps.get(0).unwrap().end() - 1;
        let Some(close) = matching(&cleaned, open) else {
            continue;
        };
        if let Some(t) = recover_body(&cleaned[open + 1..close]) {
            out.insert(tname, t);
        }
    }

    // Regex fallback: recover `CREATE UNIQUE INDEX ... ON t (cols)` the parser may have dropped.
    // Only plain-column lists on already-known tables are added (expression indexes are skipped).
    static IDX_RE: std::sync::LazyLock<Regex> = std::sync::LazyLock::new(|| {
        Regex::new(r"(?is)create\s+unique\s+index\b.*?\bon\s+([A-Za-z_][\w.]*)[^(]*\(([^)]*)\)")
            .unwrap()
    });
    static PLAIN_COL: std::sync::LazyLock<Regex> =
        std::sync::LazyLock::new(|| Regex::new(r"^[a-z_]\w*$").unwrap());
    for caps in IDX_RE.captures_iter(&cleaned) {
        let tn = caps[1]
            .rsplit('.')
            .next()
            .unwrap_or(&caps[1])
            .to_lowercase();
        let raw: Vec<&str> = caps[2].split(',').collect();
        let cols: Vec<String> = raw
            .iter()
            .map(|c| {
                c.chars()
                    .filter(|ch| *ch != '"' && !ch.is_whitespace())
                    .collect::<String>()
                    .to_lowercase()
            })
            .filter(|c| PLAIN_COL.is_match(c))
            .collect();
        // Require every listed part to be a plain column (a mix means we misread — skip, don't guess).
        if cols.len() == raw.len() && !cols.is_empty() {
            if let Some(t) = out.get_mut(&tn) {
                t.keys.push(cols);
            }
        }
    }
    // Apply parser-recognised unique indexes (any statement order).
    for (tn, key) in indexes {
        if let Some(t) = out.get_mut(&tn) {
            t.keys.push(key);
        }
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
    }

    let mut dropped = 0usize;
    for t in out.values_mut() {
        let have: BTreeSet<&str> = t.cols.iter().map(|c| c.name.as_str()).collect();
        let before = t.keys.len();
        t.keys
            .retain(|k| k.iter().all(|c| have.contains(c.as_str())));
        dropped += before - t.keys.len();
    }
    (out, dropped)
}
