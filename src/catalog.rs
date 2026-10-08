// Unless explicitly stated otherwise all files in this repository are licensed under the
// Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/).
// Copyright 2026-Present Datadog, Inc.

//! The table catalog, built from the `CREATE TABLE` statements in the input.

use std::collections::HashMap;

use sqlparser::ast::{
    visit_expressions, ColumnDef, ColumnOption, ConstraintCharacteristics, DataType, DeferrableInitial, Expr,
    FunctionArguments, IndexColumn, ObjectName, ObjectNamePart, Query, Statement, TableConstraint,
};

use crate::collation::Collation;
use crate::error::{schema, unsupported, Result};
use crate::infer::Ty;
use crate::types::{map_type, opaque_identity, IDENTITY_OPAQUE};

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

/// A table's columns (name, prover type), per-column nullability, and key column-sets (from UNIQUE /
/// PRIMARY KEY).
///
/// A column's name is the one Postgres stores: an unquoted declaration folded to lower case, a quoted
/// one as written (`dml::fold_ident`). The table's own name is lower-cased whatever its quoting.
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
    /// Parallel to `cols`: `true` only where the column is the opaque VARBINARY and its declared
    /// Postgres type has an `=` that is identity ([`opaque_identity`]), as `bytea` and `uuid` do and
    /// `double precision` and `jsonb` do not; its type in `cols` is then [`IDENTITY_OPAQUE`]. Emitted
    /// in the schema for `sqleq-solver`, which then reads `=` on the column as identity.
    ///
    /// Direction matters for soundness, as for [`Table::nullable`]: a false `true` licenses
    /// substituting values that `=` calls equal and a cast tells apart, while a false `false`
    /// merely costs proofs. So it is `false` for a catalog built without DDL to read.
    pub opaque_identity: Vec<bool>,
    /// Column sets the DDL declares unique: every `PRIMARY KEY` and `UNIQUE` constraint that holds
    /// at every statement (see [`enforced_per_statement`]). A `DEFERRABLE` one does not, so it is
    /// not here.
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
    /// Parallel to `cols`: each column's declared collation, [`Collation::Default`] where the DDL
    /// names none. A column under a collation the IR cannot carry has the type
    /// [`COLLATED`][crate::collation::COLLATED] instead; see [`crate::collation`].
    pub collations: Vec<Collation>,
}

impl Table {
    /// The keys a prover may be told about: those whose every column is `NOT NULL`.
    ///
    /// A key tells a prover that two rows agreeing on its columns are the same row. Postgres allows
    /// any number of rows whose `UNIQUE` columns hold a NULL, so a key with a nullable column is a
    /// premise Postgres does not grant: with a nullable unique `u`, `SELECT u` would be
    /// `SELECT DISTINCT u`, and on two NULL rows it is not. Such a key is dropped, not weakened, which
    /// costs only proofs. A `PRIMARY KEY`'s columns are `NOT NULL` by definition, so it always stays.
    pub fn not_null_keys(&self) -> impl Iterator<Item = &Vec<usize>> {
        self.keys
            .iter()
            .filter(|k| !k.is_empty() && k.iter().all(|&i| !self.nullable.get(i).copied().unwrap_or(true)))
    }
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

    /// Refuse a catalog in which two tables, or two columns of one table, have names that differ
    /// only in case.
    ///
    /// The catalog folds every table name to lower case, quoted or not, and a table is found by the
    /// folded name. For an unquoted name that is Postgres's own rule, but a quoted one keeps its
    /// case: `"T"` and `t` are two tables, and finding both in one slot makes `FROM "T"` lower like
    /// `FROM t`. Where no two names collide, folding loses nothing a query that runs could need,
    /// because a reference whose case differs from the declaration fails in Postgres.
    ///
    /// A column keeps a quoted name's case, and name resolution and type inference both tell `"S"`
    /// from `s`. Two columns of one table that differ only in case are refused all the same: that
    /// is no longer what keeps them apart, and lifting it is a completeness change of its own.
    ///
    /// A catalog synthesized by type inference is not checked here: `infer::build_inferred`
    /// refuses a pair that names two tables up to case, and a synthesized table may hold two such
    /// columns, since the pair's own references, compared exactly, are what named them.
    pub fn check_case_collisions(&self) -> Result<()> {
        let mut tables = std::collections::HashSet::new();
        for t in &self.tables {
            if !tables.insert(t.name.as_str()) {
                return Err(unsupported(format!("two tables named {} up to case", t.name)));
            }
            let mut cols = std::collections::HashSet::new();
            if let Some((c, _)) = t.cols.iter().find(|(c, _)| !cols.insert(c.to_lowercase())) {
                return Err(unsupported(format!("two columns of {} named {c} up to case", t.name)));
            }
        }
        Ok(())
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

/// The column name referenced by an index column, folded as a column's name is (see [`Table`]), if
/// it's a plain identifier.
pub(crate) fn index_col_name(ic: &IndexColumn) -> Option<String> {
    match &ic.column.expr {
        Expr::Identifier(id) => Some(crate::dml::fold_ident(id)),
        Expr::CompoundIdentifier(p) => p.last().map(crate::dml::fold_ident),
        _ => None,
    }
}

/// Whether a `PRIMARY KEY` or `UNIQUE` constraint with these characteristics holds after every
/// statement, which is what a key told to a prover claims: no state a query can observe has two
/// rows agreeing on it.
///
/// Only the default, `NOT DEFERRABLE`, does. A `DEFERRABLE` constraint is checked when the
/// transaction commits if it is `INITIALLY DEFERRED` (which implies `DEFERRABLE`), or once a
/// transaction runs `SET CONSTRAINTS ... DEFERRED` if it is `INITIALLY IMMEDIATE`; until then a
/// query sees the duplicates. `NOT ENFORCED` is not accepted on a key by Postgres, and a constraint
/// that says it is not enforced is no premise either. The `NOT NULL` a `PRIMARY KEY` implies is a
/// separate constraint, enforced at once whatever the key's deferrability, so it is kept.
pub(crate) fn enforced_per_statement(c: Option<&ConstraintCharacteristics>) -> bool {
    c.is_none_or(|c| {
        c.deferrable != Some(true)
            && c.initially != Some(DeferrableInitial::Deferred)
            && c.enforced != Some(false)
    })
}

/// Parse a `declare scalar|aggregate function NAME(args) returns TYPE;` DSL line into
/// `(uppercased name, declaration)`.
///
/// The keywords are found in an ASCII-lowercased copy of the line, which has the line's byte offsets,
/// so an offset found in one slices the other. A full Unicode lowercasing does not: `İ` grows from two
/// bytes to three and the Kelvin sign shrinks from three to one, and every offset after one of them
/// would land in the wrong place, or inside a character. The keywords are ASCII, as SQL's are.
pub fn parse_declare(line: &str) -> Option<(String, FnDecl)> {
    let low = line.to_ascii_lowercase();
    let fn_kw = low.find("function")?;
    let after_fn = fn_kw + "function".len();
    let rest = &line[after_fn..];
    let name: String =
        rest.trim_start().chars().take_while(|c| *c != '(' && !c.is_whitespace()).collect();
    // The last `returns` standing as a word: one inside the name, an argument or the type is not
    // the keyword.
    let word = |i: usize| {
        let before = low[..i].chars().next_back().is_some_and(|c| c.is_whitespace() || c == ')');
        let after = low[i + "returns".len()..].chars().next().is_some_and(char::is_whitespace);
        before && after
    };
    let ret_kw = low[after_fn..]
        .match_indices("returns")
        .map(|(i, _)| i + after_fn)
        .filter(|&i| word(i))
        .last()?
        + "returns".len();
    let ret: String = line[ret_kw..].trim().trim_end_matches(';').trim().to_uppercase();
    if name.is_empty() || ret.is_empty() {
        return None;
    }
    // `declare aggregate function ...` vs `declare scalar function ...`. The distinction is
    // load-bearing: an aggregate lowered as a scalar becomes a per-row function over the input,
    // which silently changes the row count.
    let aggregate = low[..fn_kw].contains("aggregate");
    let ret = crate::types::normalize_type_name(&ret);
    Some((name.to_uppercase(), FnDecl { ret, aggregate }))
}

/// The domains a DDL creates (`CREATE DOMAIN d AS numeric`), each by the name a column's type spells
/// it with, as the type it is over; `None` for a name read as no domain.
///
/// A domain's `=`, its operators and its casts are its base type's: Postgres resolves an operator over
/// a domain value as over the base type, so a domain over `numeric` is a `numeric` to every operation,
/// `2.0 = 2.00` and `CAST(x AS TEXT)` included. So both readers of a `CREATE TABLE` type a column of
/// one as its base type ([`Domains::resolve`]). What a domain adds, a `CHECK` or a `NOT NULL`, only
/// narrows the values the column holds, and reading the column as the base type quantifies over more
/// instances than Postgres has, which costs proofs and never makes one. (A domain's `DEFAULT` is not
/// read, as no column's default is outside the `INSERT` reduction's guard.)
///
/// Not read as a domain, so that a column of it keeps the domain's name, a type the frontend does not
/// know: a name created twice, a domain with a `COLLATE` (under which its `=` need not be its base
/// type's), and a name Postgres predefines a type under ([`PG_CATALOG_TYPES`]), which it resolves to
/// `pg_catalog`'s type first.
pub struct Domains(HashMap<String, Option<DataType>>);

/// The types Postgres 17 predefines in `pg_catalog`, but arrays: the names a domain of the same name
/// does not shadow ([`Domains`]).
const PG_CATALOG_TYPES: [&str; 80] = [
    "aclitem", "bit", "bool", "box", "bpchar", "bytea", "char", "cid", "cidr", "circle", "date", "datemultirange",
    "daterange", "float4", "float8", "gtsvector", "inet", "int2", "int4", "int4multirange", "int4range", "int8",
    "int8multirange", "int8range", "interval", "json", "jsonb", "jsonpath", "line", "lseg", "macaddr", "macaddr8",
    "money", "name", "numeric", "nummultirange", "numrange", "oid", "path", "pg_brin_bloom_summary",
    "pg_brin_minmax_multi_summary", "pg_dependencies", "pg_lsn", "pg_mcv_list", "pg_ndistinct", "pg_node_tree",
    "pg_snapshot", "point", "polygon", "refcursor", "regclass", "regcollation", "regconfig", "regdictionary",
    "regnamespace", "regoper", "regoperator", "regproc", "regprocedure", "regrole", "regtype", "text", "tid", "time",
    "timestamp", "timestamptz", "timetz", "tsmultirange", "tsquery", "tsrange", "tstzmultirange", "tstzrange",
    "tsvector", "txid_snapshot", "uuid", "varbit", "varchar", "xid", "xid8", "xml",
];

/// The key a type's name is looked up under: its last part, lower-cased, as a table's is.
fn domain_key(name: &ObjectName) -> String {
    obj_name(name).rsplit('.').next().unwrap_or_default().to_lowercase()
}

impl Domains {
    /// The domains `statements` create.
    pub fn of<'a>(statements: impl IntoIterator<Item = &'a Statement>) -> Domains {
        let mut out: HashMap<String, Option<DataType>> = HashMap::new();
        for st in statements {
            let Statement::CreateDomain(d) = st else { continue };
            let key = domain_key(&d.name);
            let usable = d.collation.is_none() && !PG_CATALOG_TYPES.contains(&key.as_str());
            let base = usable.then(|| d.data_type.clone());
            out.entry(key).and_modify(|b| *b = None).or_insert(base);
        }
        Domains(out)
    }

    /// The type a column declared as `dt` holds: `dt`, or the type the domain it names is over,
    /// followed through a domain over a domain.
    pub fn resolve<'a>(&'a self, mut dt: &'a DataType) -> &'a DataType {
        // A domain over itself is not one Postgres creates; the bound keeps such an input finite,
        // and leaves its column the domain's name.
        for _ in 0..=self.0.len() {
            let DataType::Custom(name, modifiers) = dt else { break };
            match self.0.get(&domain_key(name)) {
                Some(Some(base)) if modifiers.is_empty() => dt = base,
                _ => break,
            }
        }
        dt
    }
}

/// Build the catalog from the input's `CREATE TABLE`s.
///
/// Split from [`collect_queries`], which used to be the same pass, because the DML reduction sits
/// between them: it needs the target table's full column list (see [`dml`][crate::dml]), so the
/// catalog has to exist while the tree still holds `DELETE`/`UPDATE` statements rather than the two
/// queries they reduce to.
pub fn scan_ddl(statements: &[Statement]) -> Catalog {
    let mut catalog = Catalog { tables: Vec::new() };
    let created = crate::collation::created(statements);
    let domains = Domains::of(statements);
    for st in statements {
        let Statement::CreateTable(ct) = st else { continue };
        let tname = obj_name(&ct.name).to_lowercase();
        let mut cols = Vec::new();
        let mut nullable: Vec<bool> = Vec::new();
        let mut identity: Vec<bool> = Vec::new();
        let mut determined: Vec<bool> = Vec::new();
        let mut keys: Vec<Vec<usize>> = Vec::new();
        let mut collations: Vec<Collation> = Vec::new();
        for c in &ct.columns {
            let cname = crate::dml::fold_ident(&c.name);
            let data_type = domains.resolve(&c.data_type);
            let (cty, collation) = crate::collation::column(&c.options, map_type(data_type), &created);
            let idx = cols.len();
            // A collated column is `COLLATED`, never identity. The list `sqleq-solver` reads is
            // `opaque_identity`'s alone: `IDENTITY_OPAQUE` also names a type with no `=`.
            identity.push(cty == IDENTITY_OPAQUE && opaque_identity(&data_type.to_string()));
            cols.push((cname, cty));
            collations.push(collation);
            nullable.push(true);
            determined.push(row_determined(c));
            for opt in &c.options {
                match &opt.option {
                    ColumnOption::Unique(u) => {
                        if enforced_per_statement(u.characteristics.as_ref()) {
                            keys.push(vec![idx]);
                        }
                    }
                    // `PRIMARY KEY` is a key *and* implies `NOT NULL`; the second holds even when
                    // the first is deferrable.
                    ColumnOption::PrimaryKey(pk) => {
                        if enforced_per_statement(pk.characteristics.as_ref()) {
                            keys.push(vec![idx]);
                        }
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
            // UNIQUE and PRIMARY KEY both become a key column-set, unless deferrable; only PRIMARY
            // KEY also implies its columns are NOT NULL, deferrable or not.
            let (key_cols, implies_not_null, enforced): (&[IndexColumn], bool, bool) = match con {
                TableConstraint::Unique(uc) => {
                    (&uc.columns, false, enforced_per_statement(uc.characteristics.as_ref()))
                }
                TableConstraint::PrimaryKey(pk) => {
                    (&pk.columns, true, enforced_per_statement(pk.characteristics.as_ref()))
                }
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
                if enforced {
                    keys.push(set);
                }
            }
        }
        catalog.tables.push(Table {
            name: tname,
            n_declared: cols.len(),
            cols,
            nullable,
            opaque_identity: identity,
            row_determined: determined,
            keys,
            collations,
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
            t.opaque_identity.push(false);
            // A system column is never written, so no `INSERT` can omit it; the value is the
            // conservative one either way.
            t.row_determined.push(false);
            t.collations.push(Collation::Default);
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
