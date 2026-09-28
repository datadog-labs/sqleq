// Unless explicitly stated otherwise all files in this repository are licensed under the
// Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/).
// Copyright 2026-Present Datadog, Inc.

//! The table catalog, built from the `CREATE TABLE` statements in the input.

use std::collections::HashMap;

use sqlparser::ast::{
    visit_expressions, ColumnDef, ColumnOption, Expr, FunctionArguments, IndexColumn, ObjectName,
    ObjectNamePart, Query, Statement, TableConstraint,
};

use crate::error::{schema, Result};
use crate::infer::Ty;
use crate::types::map_type;

/// Columns Postgres puts on every table and no DDL ever declares.
///
/// A query is entitled to read these, so a reference to one is neither evidence that the schema is
/// incomplete nor something the resolver may refuse. [`add_system_columns`] reads this list to
/// avoid the second mistake, and it is the list any schema-completeness check must read to avoid
/// the first. One list, because the whole bug this closes was two instruments disagreeing about
/// what a table offers.
///
/// Only the columns present on *every* relation are listed: `oid` is not, since PG12 it exists on
/// system catalogs alone, and treating it as universal would suppress a real finding on a user
/// table.
/// Re-exported under the `internals` feature, which makes this doc "public"; the links
/// below point at private callers on purpose and resolve in the crate's own docs.
#[allow(rustdoc::private_intra_doc_links)]
pub const SYSTEM_COLUMNS: [&str; 6] = ["tableoid", "xmin", "cmin", "xmax", "cmax", "ctid"];

/// A table's columns (lowercased name, prover type), per-column nullability, and key column-sets
/// (from UNIQUE / PRIMARY KEY).
#[derive(Clone)]
pub struct Table {
    pub name: String,
    pub cols: Vec<(String, String)>,
    /// Parallel to `cols`: `false` only where the DDL proves the column cannot be NULL.
    ///
    /// Direction matters for soundness. `NOT NULL` *shrinks* the space of instances the prover
    /// quantifies over, so claiming it falsely could turn a non-equivalence into a `provable`.
    /// Nullable is therefore the default, and this is set `false` only for an explicit `NOT NULL`
    /// or a `PRIMARY KEY` (which implies it). Missing the constraint merely costs completeness.
    pub nullable: Vec<bool>,
    pub keys: Vec<Vec<usize>>,
    /// Parallel to `cols`: `true` only where the DDL proves the stored value is a function of the
    /// row as written, rather than of the row's *position* in the statement.
    ///
    /// Read by the `INSERT` reduction and by nothing else. `T := T ⊎ S` compares the rows as
    /// *written*, which is an iff only when the row as *stored* is recoverable from them: a
    /// `nextval()` default is a function of position, so
    /// `INSERT INTO t (name) VALUES ('x'),('y')` and `VALUES ('y'),('x')` are equal as source bags
    /// and leave different tables. Columns the statement names are written explicitly and so are
    /// never consulted; only an *omitted* column has to be checked.
    ///
    /// Direction matters for soundness, the opposite way round from [`Table::nullable`]. A false
    /// `false` costs a refusal; a false `true` licenses a proof. So this is `true` only for a
    /// column with no default, a literal default, or one of the statement-stable clock functions
    /// (see [`row_determined`]) — and `false` for a catalog built without DDL to read.
    pub row_determined: Vec<bool>,
    /// How many of `cols` the DDL declared. The rest are the system columns
    /// [`add_system_columns`] appended, and the split matters because `cols` means two things:
    ///
    /// * *width* — a system column occupies a de-Bruijn level like any other, so the offset
    ///   arithmetic, the level→binding lookup, `try_resolve` and the emitted schema must all count
    ///   `cols.len()`, or an index runs past the end of the row;
    /// * *visibility* — `SELECT *`, `t.*` and a derived table's output shape must expand to the
    ///   declared prefix only, because Postgres does not return `ctid` for a `*`.
    ///
    /// Equal to `cols.len()` on every catalog until `add_system_columns` runs.
    pub n_declared: usize,
}

/// All tables declared by the input's `CREATE TABLE`s, in declaration order (the scan index).
pub struct Catalog {
    pub tables: Vec<Table>,
}

/// A function declared by the `declare ... function` DSL: its return type, and whether it is an
/// aggregate (`declare aggregate function`) rather than a per-row scalar.
#[derive(Clone, Debug)]
pub struct FnDecl {
    pub ret: String,
    pub aggregate: bool,
}

impl Catalog {
    /// Index of a table by (case-insensitive) name.
    pub fn find(&self, name: &str) -> Option<usize> {
        let n = name.to_lowercase();
        self.tables.iter().position(|t| t.name == n)
    }
}

/// Render a (possibly qualified) object name as a dotted string.
pub fn obj_name(n: &ObjectName) -> String {
    n.0.iter()
        .filter_map(|p| match p {
            ObjectNamePart::Identifier(id) => Some(id.value.clone()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join(".")
}

/// The (lowercased) column name referenced by an index column, if it's a plain identifier.
fn index_col_name(ic: &IndexColumn) -> Option<String> {
    match &ic.column.expr {
        Expr::Identifier(id) => Some(id.value.to_lowercase()),
        Expr::CompoundIdentifier(p) => Some(p.last().unwrap().value.to_lowercase()),
        _ => None,
    }
}

/// Parse a `declare scalar|aggregate function NAME(args) returns TYPE;` DSL line into
/// `(uppercased name, declaration)`.
pub fn parse_declare(line: &str) -> Option<(String, FnDecl)> {
    let low = line.to_lowercase();
    let after_fn = low.find("function")? + "function".len();
    let rest = &line[after_fn..];
    let name: String =
        rest.trim_start().chars().take_while(|c| *c != '(' && !c.is_whitespace()).collect();
    let ret_kw = low.find("returns")? + "returns".len();
    let ret: String = line[ret_kw..].trim().trim_end_matches(';').trim().to_uppercase();
    if name.is_empty() || ret.is_empty() {
        return None;
    }
    // `declare aggregate function ...` vs `declare scalar function ...`. The distinction is
    // load-bearing: an aggregate lowered as a scalar becomes a per-row function over the input,
    // which silently changes the row count.
    let aggregate = low[..low.find("function")?].contains("aggregate");
    let ret = crate::types::normalize_type_name(&ret);
    Some((name.to_uppercase(), FnDecl { ret, aggregate }))
}

/// Build the catalog from the input's `CREATE TABLE`s.
///
/// Split from [`collect_queries`], which used to be the same pass, because the DML reduction sits
/// between them: it needs the target table's full column list (see [`dml`][crate::dml]), so the
/// catalog has to exist while the tree still holds `DELETE`/`UPDATE` statements rather than the two
/// queries they reduce to.
pub fn scan_ddl(statements: &[Statement]) -> Catalog {
    let mut catalog = Catalog { tables: Vec::new() };
    for st in statements {
        let Statement::CreateTable(ct) = st else { continue };
        let tname = obj_name(&ct.name).to_lowercase();
        let mut cols = Vec::new();
        let mut nullable: Vec<bool> = Vec::new();
        let mut determined: Vec<bool> = Vec::new();
        let mut keys: Vec<Vec<usize>> = Vec::new();
        for c in &ct.columns {
            let cname = c.name.value.to_lowercase();
            let cty = map_type(&c.data_type);
            let idx = cols.len();
            cols.push((cname, cty));
            nullable.push(true);
            determined.push(row_determined(c));
            for opt in &c.options {
                match opt.option {
                    ColumnOption::Unique { .. } => keys.push(vec![idx]),
                    // `PRIMARY KEY` is a key *and* implies `NOT NULL`.
                    ColumnOption::PrimaryKey(_) => {
                        keys.push(vec![idx]);
                        nullable[idx] = false;
                    }
                    ColumnOption::NotNull => nullable[idx] = false,
                    _ => {}
                }
            }
        }
        let name_index: HashMap<String, usize> =
            cols.iter().enumerate().map(|(i, (n, _))| (n.clone(), i)).collect();
        for con in &ct.constraints {
            // UNIQUE and PRIMARY KEY both become a key column-set; only PRIMARY KEY also implies its
            // columns are NOT NULL.
            let (key_cols, implies_not_null): (&[IndexColumn], bool) = match con {
                TableConstraint::Unique(uc) => (&uc.columns, false),
                TableConstraint::PrimaryKey(pk) => (&pk.columns, true),
                _ => continue,
            };
            let set: Vec<usize> = key_cols
                .iter()
                .filter_map(|ic| index_col_name(ic).and_then(|n| name_index.get(&n).copied()))
                .collect();
            if !set.is_empty() {
                if implies_not_null {
                    // Each PK column is individually NOT NULL, so resolving only some of a composite
                    // key still settles the ones we resolved.
                    for &i in &set {
                        nullable[i] = false;
                    }
                }
                keys.push(set);
            }
        }
        catalog.tables.push(Table {
            name: tname,
            n_declared: cols.len(),
            cols,
            nullable,
            row_determined: determined,
            keys,
        });
    }
    catalog
}

/// Is this column's stored value a function of the row as written?
///
/// See [`Table::row_determined`] for what reads it and why the direction of the answer is the
/// soundness question. `false` is always safe, so everything not recognised here is `false`.
pub fn row_determined(c: &ColumnDef) -> bool {
    // `SERIAL` and friends are sugar for a `nextval()` default, and sqlparser keeps them as a
    // custom type name rather than desugaring them, so the type has to be read as well as the
    // options.
    let ty = format!("{}", c.data_type).to_lowercase();
    if matches!(
        ty.as_str(),
        "serial" | "serial2" | "serial4" | "serial8" | "smallserial" | "bigserial"
    ) {
        return false;
    }
    for opt in &c.options {
        match &opt.option {
            // `GENERATED ... AS IDENTITY` is a sequence. `GENERATED ... AS (expr) STORED` really is
            // a function of the row, but nothing in the corpus needs the distinction and refusing
            // both keeps the rule one line long.
            ColumnOption::Generated { .. } => return false,
            ColumnOption::Default(e) if !stable_default(e) => return false,
            _ => {}
        }
    }
    true
}

/// A `DEFAULT` expression that takes the same value for every row of one statement.
fn stable_default(e: &Expr) -> bool {
    match e {
        Expr::Value(_) => true,
        // A cast or a sign over a literal is still a literal; neither reads the row.
        Expr::UnaryOp { expr, .. } | Expr::Cast { expr, .. } => stable_default(expr),
        // The clock functions take one value per statement, and the pipeline already treats such a
        // value as a constant shared by both sides -- that is what makes `reflexive` sound on a
        // query containing `now()`. Anything else that is called per row -- `nextval`,
        // `gen_random_uuid`, `random` -- is a function of position rather than of the row.
        Expr::Function(f) => {
            let no_args = match &f.args {
                FunctionArguments::None => true,
                FunctionArguments::List(l) => l.args.is_empty(),
                FunctionArguments::Subquery(_) => false,
            };
            no_args
                && matches!(
                    obj_name(&f.name).to_lowercase().as_str(),
                    "now"
                        | "current_timestamp"
                        | "current_date"
                        | "current_time"
                        | "localtimestamp"
                        | "localtime"
                        | "transaction_timestamp"
                        | "statement_timestamp"
                )
        }
        _ => false,
    }
}

/// Append the Postgres system columns the queries actually name to every table in the catalog.
///
/// A schema check that reads [`SYSTEM_COLUMNS`] treats a table as having `ctid` whether or not the
/// DDL says so. Without this the resolver did not, so real pairs refused on `unresolved column
/// ctid` against a schema that had, in the same binary, just been certified complete. That
/// asymmetry between two instruments reading one catalog is what this closes.
///
/// Appending to *every* table rather than to the one the reference belongs to is deliberate. It
/// needs no attribution pass, so it behaves the same in all three [`crate::CatalogSource`] modes,
/// and the extra column is unobservable: nothing constrains it, and `n_declared` keeps it out of
/// `*`. A pair that names no system column gets a byte-identical catalog.
///
/// Every choice below takes the conservative direction, for the reason [`crate::infer`]'s
/// `build_inferred` states: nullability and keys both *shrink* the space of instances the prover
/// quantifies over, so inventing either could turn a non-equivalence into a proof. `ctid` really is
/// unique within a table and really is non-null; we assert neither, and give it the opaque sort
/// that supports equality and nothing else. That costs completeness and cannot cost soundness.
pub fn add_system_columns(cat: &mut Catalog, queries: &[Query]) {
    let mut named = [false; SYSTEM_COLUMNS.len()];
    for q in queries {
        let _ = visit_expressions(q, |e| {
            let ident = match e {
                Expr::Identifier(id) => Some(&id.value),
                Expr::CompoundIdentifier(parts) => parts.last().map(|p| &p.value),
                _ => None,
            };
            if let Some(n) = ident {
                if let Some(i) = SYSTEM_COLUMNS.iter().position(|s| n.eq_ignore_ascii_case(s)) {
                    named[i] = true;
                }
            }
            std::ops::ControlFlow::<()>::Continue(())
        });
    }
    // In `SYSTEM_COLUMNS` order, not the order the queries happened to mention them: the emitted
    // schema is compared byte-for-byte across runs, and column order must not depend on query text.
    for t in &mut cat.tables {
        for (i, s) in SYSTEM_COLUMNS.iter().enumerate() {
            // `!named[i]`: only what the pair reads, so an untouched pair keeps its exact catalog.
            // The second test is for a synthesized catalog, which can already hold the name.
            if !named[i] || t.cols.iter().any(|(c, _)| c == s) {
                continue;
            }
            t.cols.push((s.to_string(), Ty::Opaque.sql().to_string()));
            t.nullable.push(true);
            // A system column is never written, so no `INSERT` can omit it; the value is the
            // conservative one either way.
            t.row_determined.push(false);
        }
    }
}

/// Collect the (exactly two) queries the input is a pair of.
///
/// Anything that is not a query is dropped here rather than refused by name: by the time this runs the
/// DML reduction has already turned every `DELETE`/`UPDATE` pair into a pair of queries and refused
/// the shapes it cannot, so what is left to drop is the `CREATE TABLE`s — and an `INSERT` or `MERGE`,
/// which has no reduction and shows up as a missing query.
pub fn collect_queries(statements: Vec<Statement>) -> Result<Vec<Query>> {
    let queries: Vec<Query> = statements
        .into_iter()
        .filter_map(|st| match st {
            Statement::Query(q) => Some(*q),
            _ => None,
        })
        .collect();
    if queries.len() != 2 {
        return Err(schema(format!("expected 2 queries, got {}", queries.len())));
    }
    Ok(queries)
}
