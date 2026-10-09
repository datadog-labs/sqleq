// Unless explicitly stated otherwise all files in this repository are licensed under the
// Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/).
// Copyright 2026-Present Datadog, Inc.

//! The table catalog, built from the input's DDL: its `CREATE TABLE`s and what follows them.

use std::collections::HashMap;

use sqlparser::ast::{
    visit_expressions, AlterColumnOperation, AlterIndexOperation, AlterTable, AlterTableOperation, ColumnDef,
    ColumnOption, ConstraintCharacteristics, CreateIndex, CreateTable, DataType, DeferrableInitial, Expr,
    FunctionArguments, GeneratedAs, IndexColumn, ObjectName, ObjectNamePart, ObjectType, Query, RenameTableNameKind, Statement, TableConstraint,
};

use crate::collation::Collation;
use crate::error::{schema, unsupported, Result};
use crate::infer::Ty;
use crate::rejected_ddl::Rejection;
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
    /// Parallel to `cols`: each column's type as the DDL spells it, rendered by sqlparser
    /// (`INTERVAL DAY`, `NUMERIC(10,2)`), and empty where no DDL declared the column. The type in
    /// `cols` drops the modifier, and an assignment applies it: the DML reductions read this to
    /// tell an `interval day` column, which keeps only the days of a value stored in it, from a
    /// plain `interval` one ([`dml`][crate::dml]).
    pub declared_types: Vec<String>,
    /// Parallel to `cols`: `false` only where the DDL proves the column cannot be NULL.
    ///
    /// Direction matters for soundness. `NOT NULL` *shrinks* the space of instances the prover
    /// quantifies over, so claiming it falsely could turn a non-equivalence into a `provable`.
    /// Nullable is therefore the default, and this is set `false` only for an explicit `NOT NULL`
    /// (in the `CREATE TABLE` or a later `SET NOT NULL`), a `PRIMARY KEY` (which implies it), a
    /// `SERIAL` or an identity column, unless a later statement drops it. Missing the constraint merely costs
    /// completeness.
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
    /// Column sets the DDL declares unique: every `PRIMARY KEY` and `UNIQUE` constraint, and every
    /// `CREATE UNIQUE INDEX` over the whole table, that holds at every statement (see
    /// [`enforced_per_statement`]). A `DEFERRABLE` one does not, so it is not here.
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
    /// (see `column_default`), or one whose domain's default is one of those — and `false` for a
    /// catalog built without DDL to read.
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
    /// The relations the DDL creates whose columns are not read: views, a table created without a
    /// column list, and a table a later statement changed past what the reader follows. Each is
    /// named as a table is (lower-cased, qualifier included).
    ///
    /// A query over one is refused, and the name still takes its place in resolving a reference, so
    /// that `FROM t` with a view `t` and a table `s.t` is not read as `s.t`.
    pub unread: Vec<String>,
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

    /// Whether the DDL creates a relation of this (case-insensitive) name, its columns read or not.
    pub fn declares(&self, name: &str) -> bool {
        let n = name.to_lowercase();
        self.find(&n).is_some() || self.unread.contains(&n)
    }

    /// Every relation's name: the tables', at their index, then the unread ones'.
    fn names(&self) -> impl Iterator<Item = &str> + Clone {
        self.tables.iter().map(|t| t.name.as_str()).chain(self.unread.iter().map(String::as_str))
    }

    /// The table a reference names: the one declared under that name, else the one its last part
    /// names as a bare name ([`Catalog::resolve_bare`]). Nothing if that is a relation whose columns
    /// are not read.
    pub fn resolve(&self, name: &str) -> Option<usize> {
        resolve(self.names(), name).filter(|&i| i < self.tables.len())
    }

    /// The table a bare name names: the one declared bare, else `public`'s, else the one table of
    /// that name declared under any schema. The last step takes the input's DDL as the tables its
    /// queries read, which assumes the session's search path finds that table. Two schemas declaring
    /// the name, and no bare or `public` table of it, resolve to nothing, and so does a name whose
    /// relation's columns are not read.
    pub fn resolve_bare(&self, bare: &str) -> Option<usize> {
        resolve_bare(self.names(), bare).filter(|&i| i < self.tables.len())
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
    /// A catalog synthesized by type inference is not checked here: type inference refuses a pair
    /// that names two tables up to case, under both inferred catalogs (`infer::infer`), and a
    /// synthesized table may hold two such columns, since the pair's own references, compared
    /// exactly, are what named them.
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

/// [`Catalog::resolve`] over relations' `names`: the position of the one `name` names.
fn resolve<'a>(names: impl Iterator<Item = &'a str> + Clone, name: &str) -> Option<usize> {
    let name = name.to_lowercase();
    match names.clone().position(|n| n == name) {
        Some(i) => Some(i),
        None => resolve_bare(names, name.rsplit('.').next().unwrap_or(&name)),
    }
}

/// [`Catalog::resolve_bare`] over relations' `names`.
fn resolve_bare<'a>(names: impl Iterator<Item = &'a str> + Clone, bare: &str) -> Option<usize> {
    let bare = bare.to_lowercase();
    let exact = |n: &str| names.clone().position(|m| m == n);
    exact(&bare).or_else(|| exact(&format!("public.{bare}"))).or_else(|| {
        let suffix = format!(".{bare}");
        let mut named = names.clone().enumerate().filter(|(_, n)| n.ends_with(&suffix));
        match (named.next(), named.next()) {
            (Some((i, _)), None) => Some(i),
            _ => None,
        }
    })
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

/// The columns of a key over `parts`, by their index in `cols`: every part a plain column of the
/// table, with no operator class and no `COLLATE`. Anything else is no key at all.
///
/// Not a key over the parts that are plain columns: a unique index on `(a, lower(b))` says nothing
/// about `a` alone, and one on `(s text_pattern_ops)` compares `s` by another `=` than the column's.
/// A `COLLATE` is an expression (`Expr::Collate`) here, so it is no plain column either, and a
/// sort order or a `NULLS FIRST` changes no `=`.
fn key_columns(parts: &[IndexColumn], cols: &[(String, String)]) -> Option<Vec<usize>> {
    if parts.is_empty() {
        return None;
    }
    parts
        .iter()
        .map(|ic| {
            let name = match &ic.column.expr {
                Expr::Identifier(id) if ic.operator_class.is_none() => crate::dml::fold_ident(id),
                Expr::CompoundIdentifier(p) if ic.operator_class.is_none() => crate::dml::fold_ident(p.last()?),
                _ => return None,
            };
            cols.iter().position(|(c, _)| *c == name)
        })
        .collect()
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
///
/// `sqleq-lean` reads keys with the same test (through `internals`): Postgres never takes a
/// deferrable constraint as an `ON CONFLICT` arbiter, so its witness model must not either.
pub fn enforced_per_statement(c: Option<&ConstraintCharacteristics>) -> bool {
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
/// instances than Postgres has, which costs proofs and never makes one.
///
/// Not read as a domain, so that a column of it keeps the domain's name, a type the frontend does not
/// know: a name created twice, a domain with a `COLLATE` (under which its `=` need not be its base
/// type's), and a name Postgres predefines a type under ([`PG_CATALOG_TYPES`]), which it resolves to
/// `pg_catalog`'s type first.
///
/// A domain's `DEFAULT` is what a column of it with no default of its own stores when an `INSERT`
/// omits it, so a `nextval()` there is the `INSERT` reduction's business as much as a column's
/// ([`Table::row_determined`]). That is read off every creation of the name, any of whose defaults,
/// or its base's, may be the one a column takes ([`Domains::volatile_default`]).
pub struct Domains {
    bases: HashMap<String, Option<DataType>>,
    /// Every creation of each name: the type it is over, and whether it has a default that is not
    /// the same for every row of a statement.
    creations: HashMap<String, Vec<(DataType, bool)>>,
    /// Names a statement the reader could not parse may have given another default (`ALTER DOMAIN`).
    altered: Vec<String>,
    /// Whether such a statement's name was not readable, so any domain's default may have changed.
    any_altered: bool,
}

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
        let mut bases: HashMap<String, Option<DataType>> = HashMap::new();
        let mut creations: HashMap<String, Vec<(DataType, bool)>> = HashMap::new();
        for st in statements {
            let Statement::CreateDomain(d) = st else { continue };
            let key = domain_key(&d.name);
            let volatile = d.default.as_ref().is_some_and(|e| !stable_default(e));
            creations.entry(key.clone()).or_default().push((d.data_type.clone(), volatile));
            let usable = d.collation.is_none() && !PG_CATALOG_TYPES.contains(&key.as_str());
            let base = usable.then(|| d.data_type.clone());
            bases.entry(key).and_modify(|b| *b = None).or_insert(base);
        }
        Domains { bases, creations, altered: Vec::new(), any_altered: false }
    }

    /// Whether a column declared as `dt`, with no default of its own, may take a default that is not
    /// the same for every row of a statement: one of a domain it names, followed through a domain
    /// over a domain and through every creation of a name.
    pub fn volatile_default(&self, dt: &DataType) -> bool {
        let mut seen: Vec<String> = Vec::new();
        let mut todo = vec![dt];
        while let Some(dt) = todo.pop() {
            let DataType::Custom(name, _) = dt else { continue };
            let key = domain_key(name);
            if seen.contains(&key) {
                continue;
            }
            let Some(made) = self.creations.get(&key) else { continue };
            if self.any_altered || self.altered.contains(&key) || made.iter().any(|(_, v)| *v) {
                return true;
            }
            todo.extend(made.iter().map(|(base, _)| base));
            seen.push(key);
        }
        false
    }

    /// The type a column declared as `dt` holds: `dt`, or the type the domain it names is over,
    /// followed through a domain over a domain.
    pub fn resolve<'a>(&'a self, mut dt: &'a DataType) -> &'a DataType {
        // A domain over itself is not one Postgres creates; the bound keeps such an input finite,
        // and leaves its column the domain's name.
        for _ in 0..=self.bases.len() {
            let DataType::Custom(name, modifiers) = dt else { break };
            match self.bases.get(&domain_key(name)) {
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
    build(&statements.iter().map(Ddl::Parsed).collect::<Vec<_>>(), TypeMap::Declared)
}

/// Which column-type mapper a catalog's input takes. The two readers still map a type nothing
/// classifies differently: a pair file keeps its name, raw DDL makes it an opaque `VARBINARY` (see
/// `pgddl::map_pg_type`). Everything else about a table is read one way, by [`build`].
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum TypeMap {
    /// A pair file's `CREATE TABLE`s: [`map_type`].
    Declared,
    /// Raw Postgres DDL: `pgddl::map_pg_type`, else [`crate::types::opaque_name`].
    Raw,
}

impl TypeMap {
    fn column_type(self, dt: &DataType) -> String {
        match self {
            TypeMap::Declared => map_type(dt),
            TypeMap::Raw => {
                let rendered = dt.to_string();
                crate::pgddl::map_pg_type(&rendered)
                    .map(str::to_string)
                    .unwrap_or_else(|| crate::types::opaque_name(&rendered).to_string())
            }
        }
    }
}

/// One statement of a DDL, in the order the input gives them: parsed, or the text of one the parser
/// rejected. Only raw DDL has a rejected one (`pgddl::parse_reporting`); a pair file is parsed whole.
#[derive(Clone, Copy)]
pub enum Ddl<'a> {
    Parsed(&'a Statement),
    Rejected(&'a str),
}

/// Build the catalog from a DDL's statements, for both inputs: the pair file's (through
/// [`scan_ddl`]) and raw DDL's (through `pgddl::parse_reporting`).
///
/// * A table is keyed on its name as declared, qualifier included, lower-cased. A reference finds it
///   by that name, or by its bare name ([`Catalog::resolve`]).
/// * A table whose columns are not all in its `CREATE TABLE` (`()`, `AS SELECT`, `LIKE`, `PARTITION
///   OF`, `INHERITS`) is not read, and neither is a view: each is [unread][Catalog::unread], and a
///   query over it is refused rather than lowered over the wrong columns.
/// * A column's declared type is kept with its domain resolved, which is what an assignment to it
///   converts to (`dml::Store`).
/// * Two spellings of one key (a column's `UNIQUE` also named in a table constraint) are one key.
///
/// **What follows a `CREATE TABLE` is read in order**, each statement finding its table as a query's
/// reference does ([`Catalog::resolve`]), so that a later statement undoes an earlier one:
///
/// * `ALTER TABLE` adds a `PRIMARY KEY` or a `UNIQUE` key, sets or drops a `NOT NULL` or a default,
///   makes a column an identity column, drops a constraint, and renames a constraint or the table.
///   Any other operation, one that changes the columns (`ADD`, `DROP` or `RENAME COLUMN`, `TYPE`)
///   included, leaves the table unread. An `ALTER TABLE` whose table does not resolve (a bare name
///   two schemas declare) leaves each table of that name unread, unless it only adds facts.
/// * `CREATE UNIQUE INDEX` adds a key over plain columns, unless it is partial or `IF NOT EXISTS`.
/// * `DROP TABLE` drops a table or view, and `DROP INDEX` the key an index of that name gave.
/// * A table another inherits from keeps no key and no `NOT NULL`: a scan of it reads the other
///   table's rows, which neither binds.
/// * A key added to a partitioned table alone (`ALTER TABLE ONLY`) is not read: the partitions,
///   whose rows the table reads, need not have it.
/// * A statement raw DDL's parser rejected is read off its head, and only ever as a loss of facts
///   about what it names (`rejected_ddl`).
///
/// The DDL is taken as what the database ran, every statement in it having succeeded. A statement
/// Postgres would refuse, such as a second `PRIMARY KEY`, is read as if it had taken effect.
pub fn build(ddl: &[Ddl], map: TypeMap) -> Catalog {
    let parsed: Vec<&Statement> = ddl
        .iter()
        .filter_map(|d| match d {
            Ddl::Parsed(st) => Some(*st),
            Ddl::Rejected(_) => None,
        })
        .collect();
    let rejected: Vec<Vec<Rejection>> = ddl
        .iter()
        .map(|d| match d {
            Ddl::Rejected(sql) => crate::rejected_ddl::read(sql),
            Ddl::Parsed(_) => Vec::new(),
        })
        .collect();
    // A domain's default is looked up when a row is inserted, so it is the last one the DDL gives
    // that counts, wherever the domain's columns are declared: domains are read off the whole DDL.
    let mut domains = Domains::of(parsed.iter().copied());
    for r in rejected.iter().flatten() {
        match r {
            Rejection::Domain(n) => domains.altered.push(domain_key(n)),
            Rejection::AnyDomain => domains.any_altered = true,
            _ => {}
        }
    }
    let created = crate::collation::created(parsed.iter().copied());
    let mut b = Builder { rels: Vec::new(), map, created, domains };
    for (d, losses) in ddl.iter().zip(rejected) {
        match d {
            Ddl::Parsed(st) => b.statement(st),
            Ddl::Rejected(_) => losses.into_iter().for_each(|r| b.rejected(r)),
        }
    }
    b.finish()
}

/// A relation the DDL creates, while [`build`] reads it: its name as a table is keyed, and its table,
/// if its columns are read.
struct Rel {
    name: String,
    table: Option<Building>,
}

/// A table while [`build`] reads the statements about it: a [`Table`] whose nullability, keys and
/// row-determined columns are worked out from what follows only at the end.
struct Building {
    /// Every field but `nullable`, `keys` and `row_determined`, which [`Building::finish`] fills.
    table: Table,
    /// Parallel to `cols`: a `NOT NULL` of the column's own. A primary key's columns are `NOT NULL`
    /// too, for as long as it stands.
    not_null: Vec<bool>,
    /// Parallel to `cols`: what an `INSERT` that omits the column stores.
    defaults: Vec<ColumnDefault>,
    /// Parallel to `cols`: whether the column's domain may give it a default that is not the same for
    /// every row of a statement ([`Domains::volatile_default`]).
    domain_default: Vec<bool>,
    keys: Vec<Key>,
    /// The names of its `CHECK`, `FOREIGN KEY` and `EXCLUDE` constraints, whose dropping changes
    /// nothing read here.
    others: Vec<String>,
    /// `PARTITION BY`: its rows are its partitions'.
    partitioned: bool,
    /// Another table inherits from it, so a scan of it reads that table's rows too.
    inherited: bool,
}

/// What an `INSERT` that omits a column stores in it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum ColumnDefault {
    /// No default of the column's own: NULL, or its domain's default.
    Absent,
    /// A default of its own, and whether it is the same for every row of a statement
    /// ([`stable_default`]). A `SERIAL` is a `nextval()` default.
    Own(bool),
    /// An identity column, or a generated one.
    Generated,
}

/// A `PRIMARY KEY` or `UNIQUE` constraint, or a unique index.
struct Key {
    cols: Vec<usize>,
    /// The constraint's or the index's name, lower-cased; `None` for an unnamed constraint, whose
    /// name Postgres generates.
    name: Option<String>,
    primary: bool,
    /// Whether it holds after every statement ([`enforced_per_statement`]). A deferrable primary key
    /// is no key, and its columns are `NOT NULL` all the same.
    enforced: bool,
    /// A unique index's, rather than a constraint's. An unnamed one's name is the one Postgres
    /// generated, which the reader does not work out, so any `DROP INDEX` may be of it.
    index: bool,
}

struct Builder {
    rels: Vec<Rel>,
    map: TypeMap,
    created: crate::collation::Created,
    domains: Domains,
}

impl Builder {
    fn names(&self) -> impl Iterator<Item = &str> + Clone {
        self.rels.iter().map(|r| r.name.as_str())
    }

    /// The relation `name` names, as a query's reference would find it.
    fn resolve(&self, name: &ObjectName) -> Option<usize> {
        resolve(self.names(), &obj_name(name))
    }

    /// The relations a statement about `name` may have touched: the one it resolves to, else each of
    /// that bare name, when two schemas declare it and neither is the bare or `public` one.
    fn targets(&self, name: &ObjectName) -> Vec<usize> {
        if let Some(i) = self.resolve(name) {
            return vec![i];
        }
        let full = obj_name(name).to_lowercase();
        let last = full.rsplit('.').next().unwrap_or(&full);
        (0..self.rels.len()).filter(|&i| self.rels[i].name.rsplit('.').next() == Some(last)).collect()
    }

    /// The table at `i`, if its columns are read.
    fn table(&mut self, i: usize) -> Option<&mut Building> {
        self.rels[i].table.as_mut()
    }

    fn unread(&mut self, i: usize) {
        self.rels[i].table = None;
    }

    /// Every table whose columns are read.
    fn tables(&mut self) -> impl Iterator<Item = &mut Building> {
        self.rels.iter_mut().filter_map(|r| r.table.as_mut())
    }

    fn statement(&mut self, st: &Statement) {
        match st {
            Statement::CreateTable(ct) => self.create_table(ct),
            Statement::CreateView(v) => self.create_unread(&v.name),
            Statement::CreateIndex(ci) => self.create_index(ci),
            Statement::AlterTable(at) => self.alter_table(at),
            Statement::Drop { object_type, names, .. } => match object_type {
                ObjectType::Table | ObjectType::View | ObjectType::MaterializedView => {
                    names.iter().for_each(|n| self.drop_relation(n))
                }
                ObjectType::Index => names.iter().for_each(|n| self.drop_index(n)),
                _ => {}
            },
            Statement::AlterIndex { name, operation: AlterIndexOperation::RenameIndex { index_name } } => {
                let (old, new) = (last_name(name), last_name(index_name));
                for t in self.tables() {
                    t.keys.iter_mut().filter(|k| k.name == old).for_each(|k| k.name = new.clone());
                }
            }
            _ => {}
        }
    }

    /// A relation named `name` whose columns are not known. One already of that exact name is the
    /// one Postgres refused to create twice, or the one `OR REPLACE` replaced: it is unread too.
    fn create_unread(&mut self, name: &ObjectName) {
        let name = obj_name(name).to_lowercase();
        match self.rels.iter().position(|r| r.name == name) {
            Some(i) => self.unread(i),
            None => self.rels.push(Rel { name, table: None }),
        }
    }

    fn create_table(&mut self, ct: &CreateTable) {
        let name = obj_name(&ct.name).to_lowercase();
        if ct.if_not_exists && self.rels.iter().any(|r| r.name == name) {
            return;
        }
        for parent in ct.inherits.iter().flatten() {
            for i in self.targets(parent) {
                if let Some(t) = self.table(i) {
                    t.inherited = true;
                }
            }
        }
        // Columns the statement does not list: a `LIKE` (which sqlparser reads, after a column, as a
        // column named `LIKE`; unquoted, the word is reserved and names no column), an `INHERITS`
        // parent's, a partition's parent's, or a query's.
        let like = ct.columns.iter().any(|c| c.name.quote_style.is_none() && c.name.value.eq_ignore_ascii_case("like"));
        let complete = !ct.columns.is_empty()
            && !like
            && ct.like.is_none()
            && ct.inherits.is_none()
            && ct.partition_of.is_none()
            && ct.query.is_none();
        if !complete {
            return self.create_unread(&ct.name);
        }
        let table = self.read_create_table(ct, name.clone());
        // A name read again replaces an unread one. A table declared twice is kept twice, so that
        // `Catalog::check_case_collisions` refuses it.
        self.rels.retain(|r| r.name != name || r.table.is_some());
        self.rels.push(Rel { name, table: Some(table) });
    }

    fn read_create_table(&self, ct: &CreateTable, name: String) -> Building {
        let mut b = Building {
            table: Table {
                name,
                cols: Vec::new(),
                declared_types: Vec::new(),
                nullable: Vec::new(),
                opaque_identity: Vec::new(),
                keys: Vec::new(),
                row_determined: Vec::new(),
                n_declared: ct.columns.len(),
                collations: Vec::new(),
            },
            not_null: Vec::new(),
            defaults: Vec::new(),
            domain_default: Vec::new(),
            keys: Vec::new(),
            others: Vec::new(),
            partitioned: ct.partition_by.is_some(),
            inherited: false,
        };
        for c in &ct.columns {
            let cname = crate::dml::fold_ident(&c.name);
            let data_type = self.domains.resolve(&c.data_type);
            let rendered = data_type.to_string();
            let (cty, collation) = crate::collation::column(&c.options, self.map.column_type(data_type), &self.created);
            let idx = b.table.cols.len();
            // A collated column is `COLLATED`, never identity, and an opaque name that records a
            // coarse `=` (`types::COARSE_OPAQUE`) is not `IDENTITY_OPAQUE` either. The list
            // `sqleq-solver` reads is `opaque_identity`'s alone: `IDENTITY_OPAQUE` also names a type
            // with no `=`.
            b.table.opaque_identity.push(cty == IDENTITY_OPAQUE && opaque_identity(&rendered));
            b.table.cols.push((cname, cty));
            b.table.declared_types.push(rendered);
            b.table.collations.push(collation);
            b.not_null.push(implies_not_null(c));
            b.defaults.push(column_default(c));
            b.domain_default.push(self.domains.volatile_default(&c.data_type));
            for opt in &c.options {
                let name = opt.name.as_ref().map(|n| n.value.to_lowercase());
                match &opt.option {
                    ColumnOption::Unique(u) => b.keys.push(Key {
                        cols: vec![idx],
                        name,
                        primary: false,
                        enforced: enforced_per_statement(u.characteristics.as_ref()),
                        index: false,
                    }),
                    // `PRIMARY KEY` is a key *and* implies `NOT NULL`; the second holds even when
                    // the first is deferrable.
                    ColumnOption::PrimaryKey(pk) => b.keys.push(Key {
                        cols: vec![idx],
                        name,
                        primary: true,
                        enforced: enforced_per_statement(pk.characteristics.as_ref()),
                        index: false,
                    }),
                    ColumnOption::NotNull => b.not_null[idx] = true,
                    ColumnOption::Check(_) | ColumnOption::ForeignKey(_) => b.others.extend(name),
                    _ => {}
                }
            }
        }
        for con in &ct.constraints {
            b.add_constraint(con);
        }
        b
    }

    /// `CREATE UNIQUE INDEX`: a key by the rule a constraint's takes ([`key_columns`]), when the
    /// index is unique over the whole table. A partial one (`WHERE`) is unique only over the rows it
    /// covers, and its `INCLUDE` columns are carried, not compared. `IF NOT EXISTS` gives no key: a
    /// relation of that name, which the reader need not know of, may already be there, and then
    /// Postgres creates nothing. A non-unique index is nothing here.
    fn create_index(&mut self, ci: &CreateIndex) {
        if !ci.unique || ci.predicate.is_some() || ci.if_not_exists {
            return;
        }
        let name = ci.name.as_ref().and_then(last_name);
        let Some(i) = self.resolve(&ci.table_name) else { return };
        let Some(t) = self.table(i) else { return };
        if let Some(cols) = key_columns(&ci.columns, &t.table.cols) {
            t.keys.push(Key { cols, name, primary: false, enforced: true, index: true });
        }
    }

    fn alter_table(&mut self, at: &AlterTable) {
        let Some(i) = self.resolve(&at.name) else {
            if !at.operations.iter().all(only_adds_facts) {
                for i in self.targets(&at.name) {
                    self.unread(i);
                }
            }
            return;
        };
        for op in &at.operations {
            let Some(t) = self.table(i) else { return };
            use AlterTableOperation as Op;
            match op {
                // A key added to a partitioned table alone is not on its partitions.
                Op::AddConstraint { constraint, .. } if !(at.only && t.partitioned) => t.add_constraint(constraint),
                Op::AddConstraint { .. } => {}
                Op::AlterColumn { column_name, op } => {
                    let name = crate::dml::fold_ident(column_name);
                    let Some(c) = t.table.cols.iter().position(|(n, _)| *n == name) else {
                        // A column the reader does not have: the table is not what it read.
                        self.unread(i);
                        continue;
                    };
                    match op {
                        AlterColumnOperation::SetNotNull => t.not_null[c] = true,
                        AlterColumnOperation::DropNotNull => t.not_null[c] = false,
                        // Postgres refuses both on an identity or a generated column, which keeps
                        // what it had.
                        AlterColumnOperation::SetDefault { value } if t.defaults[c] != ColumnDefault::Generated => {
                            t.defaults[c] = ColumnDefault::Own(stable_default(value))
                        }
                        AlterColumnOperation::DropDefault if t.defaults[c] != ColumnDefault::Generated => {
                            t.defaults[c] = ColumnDefault::Absent
                        }
                        AlterColumnOperation::SetDefault { .. } | AlterColumnOperation::DropDefault => {}
                        // Postgres takes `ADD GENERATED … AS IDENTITY` only on a `NOT NULL` column.
                        AlterColumnOperation::AddGenerated { .. } => {
                            t.defaults[c] = ColumnDefault::Generated;
                            t.not_null[c] = true;
                        }
                        AlterColumnOperation::SetDataType { .. } => self.unread(i),
                    }
                }
                Op::DropConstraint { name, .. } => t.drop_constraint(&name.value.to_lowercase()),
                Op::DropPrimaryKey { .. } => t.keys.retain(|k| !k.primary),
                Op::DropIndex { name } => {
                    let name = Some(name.value.to_lowercase());
                    t.keys.retain(|k| k.name != name)
                }
                Op::RenameConstraint { old_name, new_name } => {
                    let (old, new) = (old_name.value.to_lowercase(), new_name.value.to_lowercase());
                    for k in t.keys.iter_mut().filter(|k| k.name.as_deref() == Some(old.as_str())) {
                        k.name = Some(new.clone());
                    }
                    for o in t.others.iter_mut().filter(|o| **o == old) {
                        *o = new.clone();
                    }
                }
                // The new name is in the table's schema; a name already taken is one Postgres
                // refuses.
                Op::RenameTable { table_name: RenameTableNameKind::To(n) | RenameTableNameKind::As(n) } => {
                    let new = last_name(n).unwrap_or_default();
                    let rel = &mut self.rels[i];
                    rel.name = match rel.name.rsplit_once('.') {
                        Some((schema, _)) => format!("{schema}.{new}"),
                        None => new,
                    };
                    if let Some(t) = &mut rel.table {
                        t.table.name = rel.name.clone();
                    }
                }
                op if changes_nothing_read(op) => {}
                _ => self.unread(i),
            }
        }
    }

    /// `DROP TABLE` or `DROP VIEW` of `name`: gone, if it resolves; else each relation of that bare
    /// name may be, and is left unread.
    fn drop_relation(&mut self, name: &ObjectName) {
        match self.resolve(name) {
            Some(i) => {
                self.rels.remove(i);
            }
            None => {
                for i in self.targets(name) {
                    self.unread(i);
                }
            }
        }
    }

    /// `DROP INDEX` of `name`: the key of that name goes from whichever table has it, and so does
    /// every unnamed index's, whose generated name it may be. An index's name is its schema's, so the
    /// bare name is compared, which can only drop more keys than the database did.
    fn drop_index(&mut self, name: &ObjectName) {
        let name = last_name(name);
        for t in self.tables() {
            t.keys.retain(|k| k.name != name && !(k.index && k.name.is_none()));
        }
    }

    /// A loss of facts read off a statement the parser rejected.
    fn rejected(&mut self, r: Rejection) {
        match r {
            Rejection::Created(n) => self.create_unread(&n),
            Rejection::Altered(n) => self.targets(&n).into_iter().for_each(|i| self.unread(i)),
            Rejection::Default(n, col) => {
                let col = crate::dml::fold_ident(&col);
                for i in self.targets(&n) {
                    let Some(t) = self.table(i) else { continue };
                    match t.table.cols.iter().position(|(c, _)| *c == col) {
                        Some(c) => t.defaults[c] = ColumnDefault::Own(false),
                        None => self.unread(i),
                    }
                }
            }
            Rejection::Inherited(n) => {
                for i in self.targets(&n) {
                    if let Some(t) = self.table(i) {
                        t.inherited = true;
                    }
                }
            }
            Rejection::Dropped(n) => self.drop_relation(&n),
            Rejection::Index(n) => self.drop_index(&n),
            Rejection::AnyTable => (0..self.rels.len()).for_each(|i| self.unread(i)),
            // A constraint's index is dropped only with the constraint.
            Rejection::AnyIndex => self.tables().for_each(|t| t.keys.retain(|k| !k.index)),
            Rejection::Domain(_) | Rejection::AnyDomain => {}
        }
    }

    fn finish(self) -> Catalog {
        let mut tables = Vec::new();
        let mut unread = Vec::new();
        for r in self.rels {
            match r.table {
                Some(t) => tables.push(t.finish()),
                None => unread.push(r.name),
            }
        }
        Catalog { tables, unread }
    }
}

impl Building {
    /// A table constraint, from the `CREATE TABLE` or an `ALTER TABLE … ADD`.
    fn add_constraint(&mut self, con: &TableConstraint) {
        let lower = |n: &Option<sqlparser::ast::Ident>| n.as_ref().map(|n| n.value.to_lowercase());
        let (name, parts, primary, enforced) = match con {
            TableConstraint::Unique(uc) => {
                (lower(&uc.name), &uc.columns, false, enforced_per_statement(uc.characteristics.as_ref()))
            }
            TableConstraint::PrimaryKey(pk) => {
                (lower(&pk.name), &pk.columns, true, enforced_per_statement(pk.characteristics.as_ref()))
            }
            // The index becomes the constraint, under the constraint's name; a primary key makes its
            // columns `NOT NULL`. Postgres takes no partial or expression index here, and an index
            // the reader took no key from gives none.
            TableConstraint::PrimaryKeyUsingIndex(c) | TableConstraint::UniqueUsingIndex(c) => {
                let index = Some(c.index_name.value.to_lowercase());
                let primary = matches!(con, TableConstraint::PrimaryKeyUsingIndex(_));
                if let Some(k) = self.keys.iter_mut().find(|k| k.index && k.name == index) {
                    k.name = lower(&c.name).or(index);
                    k.index = false;
                    k.primary |= primary;
                    k.enforced &= enforced_per_statement(c.characteristics.as_ref());
                }
                return;
            }
            TableConstraint::Check(c) => return self.others.extend(lower(&c.name)),
            TableConstraint::ForeignKey(f) => return self.others.extend(lower(&f.name)),
            TableConstraint::Exclude(e) => return self.others.extend(lower(&e.name)),
            _ => return,
        };
        if let Some(cols) = key_columns(parts, &self.table.cols) {
            self.keys.push(Key { cols, name, primary, enforced, index: false });
        }
    }

    /// `DROP CONSTRAINT name`. A name the reader does not know may be an unnamed key's, which
    /// Postgres generated, or from Postgres 18 a `NOT NULL`'s, so the table keeps no key and no
    /// `NOT NULL`.
    fn drop_constraint(&mut self, name: &str) {
        if self.keys.iter().any(|k| k.name.as_deref() == Some(name)) {
            self.keys.retain(|k| k.name.as_deref() != Some(name));
        } else if self.others.iter().any(|o| o == name) {
            self.others.retain(|o| o != name);
        } else {
            self.keys.clear();
            self.not_null.iter_mut().for_each(|n| *n = false);
        }
    }

    fn finish(self) -> Table {
        let Building { mut table, not_null, defaults, domain_default, keys, inherited, .. } = self;
        let in_primary = |i: usize| keys.iter().any(|k| k.primary && k.cols.contains(&i));
        table.nullable = (0..table.cols.len()).map(|i| inherited || !(not_null[i] || in_primary(i))).collect();
        table.row_determined = defaults
            .iter()
            .zip(&domain_default)
            .map(|(d, from_domain)| match d {
                ColumnDefault::Absent => !from_domain,
                ColumnDefault::Own(stable) => *stable,
                ColumnDefault::Generated => false,
            })
            .collect();
        // Two spellings of one key are one key. Order-preserving, so the emitted schema is stable.
        let mut unique: Vec<Vec<usize>> = Vec::new();
        for k in keys.iter().filter(|k| k.enforced && !inherited) {
            let mut sorted = k.cols.clone();
            sorted.sort_unstable();
            if !unique.iter().any(|u| {
                let mut u = u.clone();
                u.sort_unstable();
                u == sorted
            }) {
                unique.push(k.cols.clone());
            }
        }
        table.keys = unique;
        table
    }
}

/// The last part of a name, lower-cased: an index's or a constraint's name as [`Key`] keeps it.
fn last_name(n: &ObjectName) -> Option<String> {
    obj_name(n).rsplit('.').next().map(str::to_lowercase)
}

/// An `ALTER TABLE` operation that cannot lose a fact the catalog holds: an added constraint or
/// `NOT NULL`, and what [`changes_nothing_read`] lists.
fn only_adds_facts(op: &AlterTableOperation) -> bool {
    matches!(
        op,
        AlterTableOperation::AddConstraint { .. }
            | AlterTableOperation::AlterColumn { op: AlterColumnOperation::SetNotNull, .. }
    ) || changes_nothing_read(op)
}

/// An `ALTER TABLE` operation that changes nothing the catalog reads: ownership, constraint
/// validation, triggers, rules, row security, replication and logging.
fn changes_nothing_read(op: &AlterTableOperation) -> bool {
    use AlterTableOperation as Op;
    matches!(
        op,
        Op::OwnerTo { .. }
            | Op::ValidateConstraint { .. }
            | Op::EnableTrigger { .. }
            | Op::DisableTrigger { .. }
            | Op::EnableAlwaysTrigger { .. }
            | Op::EnableReplicaTrigger { .. }
            | Op::EnableRule { .. }
            | Op::DisableRule { .. }
            | Op::EnableAlwaysRule { .. }
            | Op::EnableReplicaRule { .. }
            | Op::EnableRowLevelSecurity
            | Op::DisableRowLevelSecurity
            | Op::ForceRowLevelSecurity
            | Op::NoForceRowLevelSecurity
            | Op::ReplicaIdentity { .. }
            | Op::SetLogged
            | Op::SetUnlogged
    )
}

/// Whether a column's declaration makes it `NOT NULL` without saying so: a `SERIAL`, which is
/// shorthand for `integer NOT NULL DEFAULT nextval(…)`, or an identity column. A stored generated
/// column is not, and neither is a `DEFAULT nextval(…)` alone.
///
/// The `SERIAL` test is Postgres's, not [`column_default`]'s looser one, since claiming a `NOT NULL`
/// is the unsound direction: the type is one unqualified name, folded as an identifier, so `SERIAL`
/// and `"serial"` are one and a quoted `"SERIAL"` names some other type.
fn implies_not_null(c: &ColumnDef) -> bool {
    let serial = match &c.data_type {
        DataType::Custom(name, modifiers) if modifiers.is_empty() => match name.0.as_slice() {
            [ObjectNamePart::Identifier(id)] => matches!(
                crate::dml::fold_ident(id).as_str(),
                "serial" | "serial2" | "serial4" | "serial8" | "smallserial" | "bigserial"
            ),
            _ => false,
        },
        _ => false,
    };
    serial
        || c.options.iter().any(|o| {
            matches!(
                o.option,
                ColumnOption::Generated {
                    generated_as: GeneratedAs::Always | GeneratedAs::ByDefault,
                    generation_expr: None,
                    ..
                }
            )
        })
}

/// What an `INSERT` that omits the column stores, by its own declaration.
///
/// See [`Table::row_determined`] for what reads it and why the direction of the answer is the
/// soundness question: anything not recognised here as the same for every row is not.
fn column_default(c: &ColumnDef) -> ColumnDefault {
    // `GENERATED ... AS IDENTITY` is a sequence. `GENERATED ... AS (expr) STORED` really is a
    // function of the row, but nothing in the corpus needs the distinction and refusing both keeps
    // the rule one line long.
    if c.options.iter().any(|o| matches!(o.option, ColumnOption::Generated { .. })) {
        return ColumnDefault::Generated;
    }
    // `SERIAL` and friends are sugar for a `nextval()` default, and sqlparser keeps them as a custom
    // type name rather than desugaring them, so the type has to be read as well as the options.
    // Postgres reads the name quoted or not (`"serial"` is one), so the quotes are dropped.
    let ty = c.data_type.to_string().replace('"', "").to_lowercase();
    if matches!(ty.as_str(), "serial" | "serial2" | "serial4" | "serial8" | "smallserial" | "bigserial") {
        return ColumnDefault::Own(false);
    }
    match c.options.iter().find_map(|o| match &o.option {
        ColumnOption::Default(e) => Some(e),
        _ => None,
    }) {
        Some(e) => ColumnDefault::Own(stable_default(e)),
        None => ColumnDefault::Absent,
    }
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
            t.declared_types.push(String::new());
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
