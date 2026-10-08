// Unless explicitly stated otherwise all files in this repository are licensed under the
// Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/).
// Copyright 2026-Present Datadog, Inc.

//! Instance construction, execution, and result comparison on DuckDB.
//!
//! For each side we build every table under the exact (possibly schema-/catalog-qualified) name the
//! query uses, insert the shared generated rows — dropping any that violate a UNIQUE/NOT NULL
//! constraint so the instance stays *valid* — run the statement, and reduce the output to a sorted
//! multiset of canonicalised rows (bag semantics: ORDER BY alone never counts). SELECTs compare the
//! result set; DML compares the final table state.
//!
//! The statements are evaluated as DuckDB evaluates them; the session's one setting is a fixed time
//! zone ([`open_db`]), so a verdict does not depend on the machine it ran on.

use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::ops::ControlFlow;
use std::sync::LazyLock;

use regex::Regex;

use duckdb::types::Value as DVal;
use duckdb::{Config, Connection};
use sqlparser::ast::{visit_relations, ObjectName, ObjectNamePart};
use sqlparser::dialect::PostgreSqlDialect;
use sqlparser::parser::Parser;

use crate::gen::{lit, Val};
use crate::schema::{resolve, Schema, Table};

/// Final table name -> the set of qualified name-part lists the queries reference it by.
pub type Forms = BTreeMap<String, BTreeSet<Vec<String>>>;
/// Final table name -> generated rows (each row aligned with the table's columns).
pub type RowData = BTreeMap<String, Vec<Vec<Val>>>;

const CELL_SEP: char = '\u{1}';
const ROW_SEP: char = '\u{2}';

/// Key the `RETURNING` bag is filed under in a mutation's observation, alongside the `name=rows`
/// entry each table contributes. `\u{1}` cannot occur in a parsed identifier, so the key can never
/// collide with a table's, and it sorts ahead of every real name so the bag lands first.
const RET_KEY: &str = "\u{1}returning";

/// Render a qualified name as `"a"."b"."c"`.
fn qualify(parts: &[String]) -> String {
    parts
        .iter()
        .map(|p| format!("\"{p}\""))
        .collect::<Vec<_>>()
        .join(".")
}

/// Lower-cased dotted parts of an object name (identifiers only).
fn name_parts(n: &ObjectName) -> Vec<String> {
    n.0.iter()
        .filter_map(|p| match p {
            ObjectNamePart::Identifier(id) => Some(id.value.to_lowercase()),
            _ => None,
        })
        .collect()
}

/// Discover the qualified forms each known table is referenced by across the pair, so we create it
/// under exactly that name. Falls back to bare names when either query does not parse.
pub fn table_forms(a: &str, b: &str, schema: &Schema) -> Forms {
    let dialect = PostgreSqlDialect {};
    let mut forms: Forms = BTreeMap::new();
    let parsed_a = Parser::parse_sql(&dialect, a).ok();
    let parsed_b = Parser::parse_sql(&dialect, b).ok();
    if let (Some(sa), Some(sb)) = (&parsed_a, &parsed_b) {
        for st in sa.iter().chain(sb.iter()) {
            let _ = visit_relations(st, |name: &ObjectName| {
                let parts = name_parts(name);
                if let Some(key) = resolve(schema, &parts) {
                    forms.entry(key.clone()).or_default().insert(parts.clone());
                }
                ControlFlow::<()>::Continue(())
            });
        }
    }
    if forms.is_empty() {
        // Regex fallback, used when either side fails to parse. It must recover the *qualified*
        // form and not just the bare name: creating `people` when the query reads `crm.people`
        // leaves DuckDB reporting that schema "crm" does not exist, which loses the pair even
        // though we hold a perfectly good table for it.
        //
        // Quotes are stripped first so `"crm"."people"` reduces to the dotted form. The
        // leading `[^\w.]` guard stops a table name matching the tail of a longer identifier
        // (`user_products` must not match `products`) and also caps the qualifier chain at the two
        // segments `ensure_namespace` can build -- a deeper name simply does not match, which is
        // the conservative outcome.
        let text = format!("{a} {b}").to_lowercase().replace('"', "");
        for key in schema.keys() {
            // A key is `name` or, for a name declared in several schemas, `schema.name`; the name is
            // what to look for, and `resolve` decides whether the spelling found means this table.
            let name = key.rsplit('.').next().unwrap_or(key);
            let re = regex::Regex::new(&format!(
                r"(?:^|[^\w.])((?:[a-z_]\w*\.){{0,2}}){}(?:[^\w]|$)",
                regex::escape(name)
            ))
            .unwrap();
            for caps in re.captures_iter(&text) {
                let mut parts: Vec<String> = caps[1]
                    .split('.')
                    .filter(|s| !s.is_empty())
                    .map(str::to_string)
                    .collect();
                parts.push(name.to_string());
                if resolve(schema, &parts) == Some(key) {
                    forms.entry(key.clone()).or_default().insert(parts);
                }
            }
        }
    }
    forms
}

/// Build a `CREATE TABLE` carrying every uniqueness constraint (dedup'ing repeated columns within a
/// key and overlapping key column-sets). Enforcing these is what keeps generated instances valid.
pub fn ddl_for(parts: &[String], t: &Table) -> String {
    let mut cols: Vec<String> = t
        .cols
        .iter()
        .map(|c| {
            // A declared `text[]` becomes a DuckDB LIST. Generating it as a scalar VARCHAR instead
            // makes every array operator over the column unbindable, and turns any counterexample
            // the pair does find into a claim about a database the declared schema forbids.
            let ty = if c.array {
                format!("{}[]", c.vt.sql())
            } else {
                c.vt.sql()
            };
            format!(
                "\"{}\" {}{}",
                c.name,
                ty,
                if c.notnull { " NOT NULL" } else { "" }
            )
        })
        .collect();
    let mut seen: HashSet<BTreeSet<String>> = HashSet::new();
    for key in &t.keys {
        // Drop columns repeated within one key (order-preserving).
        let mut dedup: Vec<String> = Vec::new();
        let mut in_key: HashSet<String> = HashSet::new();
        for c in key {
            if in_key.insert(c.clone()) {
                dedup.push(c.clone());
            }
        }
        if dedup.is_empty() {
            continue;
        }
        let set: BTreeSet<String> = dedup.iter().cloned().collect();
        if !seen.insert(set) {
            continue; // overlapping PK / UNIQUE / UNIQUE INDEX on the same column set
        }
        let names = dedup
            .iter()
            .map(|c| format!("\"{c}\""))
            .collect::<Vec<_>>()
            .join(", ");
        cols.push(format!("UNIQUE ({names})"));
    }
    format!("CREATE TABLE {} ({})", qualify(parts), cols.join(", "))
}

/// One `CREATE UNIQUE INDEX` per expression key of `t` ([`Table::expr_keys`]), on the table named
/// by `parts`.
///
/// A unique index over expressions has no column-list form, so it is created as a DuckDB unique
/// index, which rejects a colliding row on insert just as `UNIQUE (..)` does. An index is named
/// within its table's schema, so the name carries the whole qualified table name to keep two
/// spellings of one table apart; `DROP TABLE` takes the index along.
pub fn expr_index_ddl(parts: &[String], t: &Table) -> Vec<String> {
    let qn = qualify(parts);
    let tag: String = parts
        .join("_")
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect();
    t.expr_keys
        .iter()
        .enumerate()
        .map(|(i, exprs)| {
            format!(
                "CREATE UNIQUE INDEX \"sqleq_uix_{tag}_{i}\" ON {qn} ({})",
                exprs.join(", ")
            )
        })
        .collect()
}

/// Create the table `parts` names, with every constraint of `t`. Returns the expression keys DuckDB
/// would not index -- it refuses a JSON operator in an index expression, for one -- which
/// [`insert_rows`] and [`accepted_rows`] then enforce by checking each insert.
fn create_table<'t>(
    con: &Connection,
    parts: &[String],
    t: &'t Table,
) -> duckdb::Result<Vec<&'t Vec<String>>> {
    con.execute_batch(&ddl_for(parts, t))?;
    Ok(expr_index_ddl(parts, t)
        .iter()
        .zip(&t.expr_keys)
        .filter(|(ddl, _)| con.execute_batch(ddl).is_err())
        .map(|(_, key)| key)
        .collect())
}

/// Insert one row as a unique index on `unindexed` would: inside a transaction that is rolled back
/// if the row fails a constraint DuckDB enforces, or leaves two rows equal and non-NULL on one of
/// those expression lists. Whether it went in.
///
/// Evaluating the expressions after the insert, rather than refusing the table, keeps the pair: the
/// rows the generator draws mostly leave such an expression NULL, which never collides. A key whose
/// expressions DuckDB cannot evaluate at all makes the row fail, so nothing is inserted and the
/// trial compares tables both sides see empty -- never a row Postgres would have refused.
fn insert_checked(con: &Connection, qn: &str, row: &str, unindexed: &[&Vec<String>]) -> bool {
    if con
        .execute_batch(&format!("BEGIN TRANSACTION; INSERT INTO {qn} VALUES ({row})"))
        .is_err()
    {
        let _ = con.execute_batch("ROLLBACK");
        return false;
    }
    let collides = |key: &Vec<String>| -> duckdb::Result<bool> {
        let exprs = key.join(", ");
        let present = key
            .iter()
            .map(|e| format!("{e} IS NOT NULL"))
            .collect::<Vec<_>>()
            .join(" AND ");
        con.query_row(
            &format!(
                "SELECT count(*) > 0 FROM (SELECT 1 FROM {qn} WHERE {present} \
                 GROUP BY {exprs} HAVING count(*) > 1)"
            ),
            [],
            |r| r.get(0),
        )
    };
    let ok = unindexed.iter().all(|k| matches!(collides(k), Ok(false)));
    let _ = con.execute_batch(if ok { "COMMIT" } else { "ROLLBACK" });
    ok
}

/// One database, reused for every trial of a pair. Constructing a DuckDB instance costs ~20ms —
/// far more than the 5-row queries themselves — so opening one per side per trial spent most of the
/// run in `duckdb_open`. Reuse is per *pair*, not per worker thread, so pairs stay isolated.
///
/// `threads=1` because intra-query parallelism buys nothing on 5-row tables and the workers already
/// saturate the cores; the default pool (one thread per core, per instance) only added contention.
///
/// Extension autoloading is off. DuckDB's default is to fetch a missing extension from
/// `extensions.duckdb.org` on first use, which would make a verdict depend on the network and on
/// whatever happens to be in `~/.duckdb` — the same pair could come back decided on one machine
/// and `Error` on another. The library we link against already has icu, json and parquet compiled
/// in, so nothing here needs fetching and turning it off costs no coverage. A missing extension
/// was never a soundness risk (`pair.rs` abandons a trial whose side errors, so a refutation
/// always has two successful sides), but it was a reproducibility one.
///
/// The session evaluates pairs as DuckDB does, with one setting: `TimeZone = 'UTC'`, a fixed zone
/// instead of the host's, so a verdict on a pair that converts between `timestamptz` and local time
/// does not depend on the machine it ran on.
pub fn open_db() -> duckdb::Result<Connection> {
    let config = Config::default().threads(1)?.enable_autoload_extension(false)?;
    let con = Connection::open_in_memory_with_flags(config)?;
    con.execute_batch("SET TimeZone = 'UTC';")?;
    Ok(con)
}

/// Drop everything a side created, so the next side starts from a clean catalog. `thorough` also
/// sweeps objects we did not create ourselves — a DML statement may add its own tables or views, and
/// a leftover would collide with the next trial's `CREATE TABLE`.
fn reset(con: &Connection, created: &[(String, String)], thorough: bool) {
    for (qn, _) in created {
        let _ = con.execute_batch(&format!("DROP TABLE IF EXISTS {qn}"));
    }
    if !thorough {
        return;
    }
    let listed: Vec<(String, String, String, bool)> = con
        .prepare(
            "SELECT database_name, schema_name, table_name, false FROM duckdb_tables() \
             UNION ALL SELECT database_name, schema_name, view_name, true FROM duckdb_views() \
             WHERE NOT internal",
        )
        .and_then(|mut s| {
            s.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?
                .collect::<duckdb::Result<Vec<_>>>()
        })
        .unwrap_or_default();
    for (db, sch, name, is_view) in listed {
        let qn = qualify(&[db, sch, name]);
        let kind = if is_view { "VIEW" } else { "TABLE" };
        let _ = con.execute_batch(&format!("DROP {kind} IF EXISTS {qn}"));
    }
}

/// Whether a batched multi-row `INSERT` is certain to succeed: no NULL in a NOT NULL column and no
/// duplicate within any uniqueness key. When it holds, one statement replaces N — and when it does
/// not, inserting row-by-row is what drops the offending rows and keeps the instance valid.
fn rows_all_valid(t: &Table, rows: &[Vec<Val>]) -> bool {
    for row in rows {
        for (c, v) in t.cols.iter().zip(row) {
            if c.notnull && *v == Val::Null {
                return false;
            }
        }
    }
    let idx = |name: &str| t.cols.iter().position(|c| c.name == name);
    for key in &t.keys {
        let cols: Vec<usize> = key.iter().filter_map(|c| idx(c)).collect();
        if cols.is_empty() {
            continue;
        }
        let mut seen: HashSet<Vec<String>> = HashSet::new();
        for row in rows {
            // A key holding NULL never conflicts (SQL uniqueness ignores it).
            if cols.iter().any(|&i| row[i] == Val::Null) {
                continue;
            }
            if !seen.insert(cols.iter().map(|&i| lit(&row[i])).collect()) {
                return false;
            }
        }
    }
    true
}

/// Insert the generated rows. A failing multi-row `INSERT` is atomic in DuckDB (it leaves the table
/// untouched), so falling back row-by-row reproduces exactly the survivor set the row-by-row path
/// would have produced on its own. Expression keys DuckDB would not index are checked row by row.
fn insert_rows(
    con: &Connection,
    qn: &str,
    t: &Table,
    rows: &[Vec<Val>],
    unindexed: &[&Vec<String>],
) {
    let render = |row: &Vec<Val>| row.iter().map(lit).collect::<Vec<_>>().join(", ");
    if !unindexed.is_empty() {
        for row in rows {
            insert_checked(con, qn, &render(row), unindexed);
        }
        return;
    }
    if rows_all_valid(t, rows) {
        let all = rows
            .iter()
            .map(|r| format!("({})", render(r)))
            .collect::<Vec<_>>()
            .join(", ");
        if con
            .execute_batch(&format!("INSERT INTO {qn} VALUES {all}"))
            .is_ok()
        {
            return;
        }
    }
    for row in rows {
        // A row violating UNIQUE/NOT NULL is dropped, keeping the instance valid.
        let _ = con.execute_batch(&format!("INSERT INTO {qn} VALUES ({})", render(row)));
    }
}

/// Create the schema/catalog namespace a 2-/3-part table name needs.
fn ensure_namespace(
    con: &Connection,
    parts: &[String],
    attached: &mut HashSet<String>,
) -> duckdb::Result<()> {
    if parts.len() == 2 {
        con.execute_batch(&format!("CREATE SCHEMA IF NOT EXISTS \"{}\"", parts[0]))?;
    } else if parts.len() >= 3 {
        let cat = &parts[0];
        if attached.insert(cat.clone()) {
            con.execute_batch(&format!("ATTACH IF NOT EXISTS ':memory:' AS \"{cat}\""))?;
        }
        con.execute_batch(&format!(
            "CREATE SCHEMA IF NOT EXISTS \"{}\".\"{}\"",
            parts[0], parts[1]
        ))?;
    }
    Ok(())
}

/// Cast targets written as `expr::[schema.]type` and `CAST(expr AS [schema.]type)`. The `CAST`
/// form deliberately refuses nested parentheses: a cast we cannot read cleanly is skipped, which
/// is today's behaviour, whereas a misread one would declare a type that is not there.
static CAST_OP: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"(?i)::\s*(?:"?([a-z_]\w*)"?\s*\.\s*)?"?([a-z_]\w*)"?"#).unwrap()
});
static CAST_FN: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"(?i)\bcast\s*\([^()]*?\bas\s+(?:"?([a-z_]\w*)"?\s*\.\s*)?"?([a-z_]\w*)"?\s*\)"#)
        .unwrap()
});

/// Declare cast targets DuckDB does not know.
///
/// A query may cast to a Postgres type the DDL never names — `$1::text::public.job_state`. The
/// *column* is typed by the DDL and collapsed to VARCHAR by `map_vtype`, but the cast still names a
/// type DuckDB has never heard of, and the pair is lost to a catalog error before either side runs.
///
/// The failure mode to avoid is redeclaring a real type as VARCHAR, which would silently change
/// semantics. Rather than maintain DuckDB's builtin spellings by hand, we ask DuckDB itself: a name
/// that already resolves is left alone, and so is a qualified name whose bare form resolves, so
/// `pg_catalog.int4` can never become VARCHAR (it stays unresolvable, as it is today). Only a name
/// DuckDB rejects both ways is declared. The pair's two sides share one connection, so whatever we
/// declare is identical for both — an asymmetry between A and B is not constructible here. Enum
/// ordering is lost to VARCHAR, but results compare as sorted multisets, so that can only cost a
/// counterexample, never invent one.
fn ensure_types(con: &Connection, stmt: &str) {
    let resolves = |t: &str| {
        con.execute_batch(&format!("SELECT CAST(NULL AS {t})"))
            .is_ok()
    };
    let mut seen: HashSet<String> = HashSet::new();
    for caps in CAST_OP
        .captures_iter(stmt)
        .chain(CAST_FN.captures_iter(stmt))
    {
        let mut parts: Vec<String> = Vec::new();
        if let Some(q) = caps.get(1) {
            parts.push(q.as_str().to_lowercase());
        }
        parts.push(caps[2].to_lowercase());
        let qn = qualify(&parts);
        if !seen.insert(qn.clone()) || resolves(&qn) {
            continue;
        }
        // The bare name resolving means this is a builtin under a qualifier we cannot honour.
        if parts.len() > 1 && resolves(&qualify(&parts[parts.len() - 1..])) {
            continue;
        }
        if parts.len() == 2 {
            let _ = con.execute_batch(&format!("CREATE SCHEMA IF NOT EXISTS \"{}\"", parts[0]));
        }
        let _ = con.execute_batch(&format!("CREATE TYPE {qn} AS VARCHAR"));
    }
}

/// A stable, order-insensitive rendering of a cell. List elements are *sorted* because array_agg /
/// unnest order is nondeterministic without ORDER BY — an element-order difference is not a sound
/// counterexample.
///
/// Numbers are rendered by value, not by DuckDB type. The generated schema materializes a declared
/// `bigint` as `INTEGER`, so `c` reads back as `Int(0)` while `c::bigint` reads back as `BigInt(0)`,
/// and a rendering that kept the type would call two equal values different — a false refutation,
/// the one failure this tester must not have. Rendering by value can only merge cells, never split
/// them, so it cannot manufacture a counterexample either.
///
/// The same holds inside a composite value, so a composite is rendered from its parts' renderings
/// rather than from its `Debug` text, which spells out each part's DuckDB type: `ROW(b)` over an
/// `INTEGER`-materialized `bigint` and `ROW(b::bigint)` are one Postgres record. A STRUCT keeps its
/// field order and drops its field names (a Postgres record has none to compare, and DuckDB names
/// an anonymous one's fields after their expressions); a MAP is a set of entries; a fixed-size
/// ARRAY is a LIST; a UNION is the value it holds.
///
/// An interval is rendered by the span Postgres's `=` compares (`interval_cmp`), a month counting as
/// 30 days and a day as 24 hours, not by its three fields: `INTERVAL '1 day' = INTERVAL '24 hours'`
/// in Postgres, so the two are one cell, however differently they print. Like the numbers, this can
/// only merge cells.
fn canon(v: &DVal) -> String {
    match v {
        DVal::Interval {
            months,
            days,
            nanos,
        } => {
            const DAY_NS: i128 = 86_400 * 1_000_000_000;
            let span = (*months as i128 * 30 + *days as i128) * DAY_NS + *nanos as i128;
            format!("Interval({span})")
        }
        DVal::List(items) | DVal::Array(items) => {
            let mut cs: Vec<String> = items.iter().map(canon).collect();
            cs.sort();
            format!("[{}]", cs.join(","))
        }
        DVal::Struct(fields) => {
            let cs: Vec<String> = fields.iter().map(|(_, v)| canon(v)).collect();
            format!("({})", cs.join(","))
        }
        DVal::Map(entries) => {
            let mut cs: Vec<String> = entries
                .iter()
                .map(|(k, v)| format!("{}=>{}", canon(k), canon(v)))
                .collect();
            cs.sort();
            format!("{{{}}}", cs.join(","))
        }
        DVal::Union(inner) => canon(inner),
        other => match number(other) {
            Some(n) => format!("Number({n})"),
            None => format!("{other:?}"),
        },
    }
}

/// A numeric cell's value as its shortest decimal spelling, whatever its width or representation:
/// `Int(5)`, `BigInt(5)`, `Decimal(5.00)` and `Double(5.0)` all give `5`. `None` for anything else.
fn number(v: &DVal) -> Option<String> {
    let s = match v {
        DVal::TinyInt(i) => i.to_string(),
        DVal::SmallInt(i) => i.to_string(),
        DVal::Int(i) => i.to_string(),
        DVal::BigInt(i) => i.to_string(),
        DVal::HugeInt(i) => i.to_string(),
        DVal::UTinyInt(i) => i.to_string(),
        DVal::USmallInt(i) => i.to_string(),
        DVal::UInt(i) => i.to_string(),
        DVal::UBigInt(i) => i.to_string(),
        DVal::UHugeInt(i) => i.to_string(),
        // `Display` is exact; its scale is only presentation, so `1.50` and `1.5` are one value.
        DVal::Decimal(d) => {
            let s = d.to_string();
            if s.contains('.') {
                s.trim_end_matches('0').trim_end_matches('.').to_string()
            } else {
                s
            }
        }
        // `Display` for floats is the shortest spelling that round-trips, with no exponent.
        DVal::Float(f) => f.to_string(),
        DVal::Double(f) => f.to_string(),
        _ => return None,
    };
    // Negative zero is zero, in every one of these representations.
    Some(if s == "-0" { "0".to_string() } else { s })
}

/// Read every row of `sql` as canonical `CELL_SEP`-joined strings.
fn fetch_rows(con: &Connection, sql: &str) -> duckdb::Result<Vec<String>> {
    let mut stmt = con.prepare(sql)?;
    let mut rows = stmt.query([])?;
    // Column count is only known once the statement has been executed (i.e. after `query`).
    let ncols = rows.as_ref().map(|s| s.column_count()).unwrap_or(0);
    let mut out = Vec::new();
    while let Some(row) = rows.next()? {
        let mut cells = Vec::with_capacity(ncols);
        for i in 0..ncols {
            cells.push(canon(&row.get::<usize, DVal>(i)?));
        }
        out.push(cells.join(&CELL_SEP.to_string()));
    }
    Ok(out)
}

/// Replay an instance and report, per table, the generated rows the database actually **accepted**.
///
/// [`insert_rows`] drops any row violating a UNIQUE or NOT NULL constraint, so the generated rows
/// are a superset of what the trial really ran against. A counterexample has to be reported in
/// terms of the accepted set, or the witness names rows the tested database never held -- and, in
/// the common case of a duplicate key, prints an instance the schema itself forbids. DuckDB decides
/// here exactly as it does during a trial, so the two cannot drift. Called only once a
/// counterexample is in hand, so the extra load costs nothing on the common path.
// Documented in terms of the private helper that does the inserting.
#[allow(rustdoc::private_intra_doc_links)]
pub fn accepted_rows(
    con: &Connection,
    forms: &Forms,
    schema: &Schema,
    rowdata: &RowData,
) -> duckdb::Result<RowData> {
    let mut attached: HashSet<String> = HashSet::new();
    let created: Vec<(String, String)> = forms
        .iter()
        .flat_map(|(fname, partsets)| {
            partsets
                .iter()
                .map(move |parts| (qualify(parts), fname.clone()))
        })
        .collect();
    reset(con, &created, true);

    let mut out = RowData::new();
    for (fname, partsets) in forms {
        for parts in partsets {
            ensure_namespace(con, parts, &mut attached)?;
            let unindexed = create_table(con, parts, &schema[fname])?;
            let qn = qualify(parts);
            // Row by row unconditionally: the batched path in `insert_rows` is an optimisation for
            // the case where nothing is dropped, and it is the row-by-row path that reveals which.
            let kept: Vec<Vec<Val>> = rowdata[fname]
                .iter()
                .filter(|row| {
                    let vals = row.iter().map(lit).collect::<Vec<_>>().join(", ");
                    if unindexed.is_empty() {
                        con.execute_batch(&format!("INSERT INTO {qn} VALUES ({vals})"))
                            .is_ok()
                    } else {
                        insert_checked(con, &qn, &vals, &unindexed)
                    }
                })
                .cloned()
                .collect();
            // Every form of one name is a fresh table loaded from the same rows, so each pass over
            // a repeated name computes the same survivors.
            out.insert(fname.clone(), kept);
        }
    }
    Ok(out)
}

/// Create tables, insert `rowdata`, run `stmt`, and return (comparable, row_count). `row_count` is
/// deterministic even under a truncating LIMIT, so a count difference is always a sound signal.
///
/// `con` is shared across the pair's trials, so the catalog is cleared on the way *in* — an earlier
/// side that failed mid-way may have left tables behind. `mutates` marks a statement that can create
/// objects of its own, which the cheap name-directed reset would miss.
// Eight arguments, all of them per-side facts the caller already has; bundling them into a
// struct would only move the same list one level out.
#[allow(clippy::too_many_arguments)]
pub fn run_side(
    con: &Connection,
    stmt: &str,
    is_query: bool,
    returning: bool,
    mutates: bool,
    forms: &Forms,
    schema: &Schema,
    rowdata: &RowData,
) -> duckdb::Result<(Vec<String>, usize)> {
    let mut attached: HashSet<String> = HashSet::new();
    let mut created: Vec<(String, String)> = Vec::new(); // (qualified name, final name)
    for (fname, partsets) in forms {
        for parts in partsets {
            created.push((qualify(parts), fname.clone()));
        }
    }
    reset(con, &created, mutates);
    ensure_types(con, stmt);

    for (fname, partsets) in forms {
        for parts in partsets {
            ensure_namespace(con, parts, &mut attached)?;
            let unindexed = create_table(con, parts, &schema[fname])?;
            insert_rows(
                con,
                &qualify(parts),
                &schema[fname],
                &rowdata[fname],
                &unindexed,
            );
        }
    }

    if is_query {
        let mut rows = fetch_rows(con, stmt)?;
        let size = rows.len();
        rows.sort();
        Ok((rows, size))
    } else {
        // A mutation's meaning is the *pair* (returned bag, new table state), and the two are
        // independent -- `SET c = c WHERE true RETURNING c` and `WHERE false` differ only in the
        // bag. `execute_batch` discards rows, so a statement that returns any is run through
        // `fetch_rows` instead: it performs the mutation just the same and hands the bag back
        // (verified against DuckDB 1.5.5 through this exact path). The bag is sorted because
        // `RETURNING` fixes no row order.
        let mut res: Vec<String> = Vec::new();
        let mut size = 0usize;
        if returning {
            let mut bag = fetch_rows(con, stmt)?;
            size += bag.len();
            bag.sort();
            res.push(format!("{RET_KEY}={}", bag.join(&ROW_SEP.to_string())));
        } else {
            con.execute_batch(stmt)?;
        }
        let mut seen: HashSet<String> = HashSet::new();
        for (qn, fname) in &created {
            if !seen.insert(fname.clone()) {
                continue;
            }
            let mut tbl = fetch_rows(con, &format!("SELECT * FROM {qn}"))?;
            size += tbl.len();
            tbl.sort();
            res.push(format!("{fname}={}", tbl.join(&ROW_SEP.to_string())));
        }
        res.sort();
        Ok((res, size))
    }
}

#[cfg(test)]
mod tests {
    use super::{canon, ddl_for, fetch_rows, open_db};
    use crate::gen::{lit, Val, JSONS};
    use crate::schema::{Column, Table, VType};
    use duckdb::types::Value as DVal;

    fn json_col(name: &str, array: bool) -> Column {
        Column {
            name: name.to_string(),
            vt: VType::Json,
            notnull: false,
            array,
            sequenced: false,
        }
    }

    /// A number is one cell whatever DuckDB type carries it, so an equal value in another width,
    /// scale or representation is not a counterexample. Different values, and a number against
    /// its text spelling, stay apart.
    #[test]
    fn numbers_compare_by_value_not_by_type() {
        let dec = |w, s, v| DVal::Decimal(duckdb::types::Decimal::new(w, s, v).unwrap());
        for v in [
            DVal::BigInt(0),
            DVal::HugeInt(0),
            DVal::UTinyInt(0),
            dec(10, 2, 0),
            DVal::Float(0.0),
            DVal::Double(-0.0),
        ] {
            assert_eq!(canon(&v), canon(&DVal::Int(0)), "{v:?}");
        }
        assert_eq!(canon(&dec(10, 2, 150)), canon(&DVal::Double(1.5)));
        assert_eq!(canon(&dec(12, 3, 1500)), canon(&dec(10, 1, 15)));
        assert_eq!(canon(&dec(10, 2, -500)), canon(&DVal::BigInt(-5)));
        assert_ne!(canon(&DVal::Int(1)), canon(&DVal::Int(2)));
        assert_ne!(canon(&DVal::Double(0.5)), canon(&DVal::Int(0)));
        assert_ne!(canon(&DVal::Text("1".into())), canon(&DVal::Int(1)));
        assert_eq!(
            canon(&DVal::List(vec![DVal::Int(1), DVal::BigInt(2)])),
            canon(&DVal::List(vec![DVal::BigInt(2), DVal::Int(1)]))
        );
    }

    /// A declared `json`/`jsonb` column has to reach DuckDB *as* JSON, and every document the
    /// generator draws has to survive the round trip byte-for-byte -- DuckDB compares JSON
    /// textually, so a re-serialization on insert would make a param that is spelled exactly like
    /// the column data compare unequal to it anyway.
    #[test]
    fn a_json_column_round_trips_and_its_accessors_bind() {
        let t = Table {
            cols: vec![json_col("j", false), json_col("js", true)],
            ..Table::default()
        };
        let ddl = ddl_for(&["t".to_string()], &t);
        assert!(ddl.contains("\"j\" JSON"), "{ddl}");
        assert!(ddl.contains("\"js\" JSON[]"), "{ddl}");

        // Through `open_db`, not a bare connection: with autoloading off this passes only if the
        // json extension is really compiled into the library we linked.
        let con = open_db().unwrap();
        con.execute_batch(&ddl).unwrap();
        for doc in JSONS {
            let row = Val::List(vec![Val::Str(doc.to_string())]);
            con.execute_batch(&format!(
                "INSERT INTO t VALUES ({}, {})",
                lit(&Val::Str(doc.to_string())),
                lit(&row)
            ))
            .unwrap();
        }

        // Verbatim, in both the scalar and the list column.
        let rows = fetch_rows(&con, "SELECT j, js FROM t ORDER BY j").unwrap();
        assert_eq!(rows.len(), JSONS.len());
        for doc in JSONS {
            // `canon` renders a cell with `{:?}`, so the document appears in its escaped spelling.
            let cell = format!("{doc:?}");
            assert!(
                rows.iter()
                    .filter(|r| r.matches(&cell).count() == 2)
                    .count()
                    == 1,
                "{doc} not round-tripped once in both columns: {rows:?}"
            );
        }

        // The whole point of the type: this is `Invalid Input Error: Malformed JSON at byte 0` when
        // the column is materialized as the VARCHAR `'a'`.
        let acc = fetch_rows(&con, "SELECT j ->> 'a' FROM t ORDER BY 1").unwrap();
        assert_eq!(acc.len(), JSONS.len());
        assert_eq!(
            acc.iter().filter(|c| c.contains("Null")).count(),
            2,
            "each key is absent from exactly two pool documents: {acc:?}"
        );
    }

    /// The other extension the linked library has to carry, and the one the crates.io `bundled`
    /// build could not supply at all. `gen.rs` generates named time zones for any argument that
    /// needs one, so without icu every pair that uses one comes back `Error` — and with DuckDB's
    /// default settings it would instead be silently fetched over the network on first use.
    #[test]
    fn a_named_time_zone_resolves_without_fetching_an_extension() {
        let con = open_db().unwrap();
        let rows = fetch_rows(
            &con,
            "SELECT (TIMESTAMP '2024-01-15 12:00:00' AT TIME ZONE 'America/New_York')::VARCHAR",
        )
        .unwrap();
        // New York is five hours behind UTC in January, so this reads icu's actual tz database
        // rather than merely parsing the clause.
        assert!(
            rows.len() == 1 && rows[0].contains("2024-01-15 17:00:00"),
            "{rows:?}"
        );
    }
}
