// Unless explicitly stated otherwise all files in this repository are licensed under the
// Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/).
// Copyright 2026-Present Datadog, Inc.

//! The target tables' columns, read from DDL, at the precision the Lean checker needs.
//!
//! The frontend's catalog is not enough here: it collapses every type to one of five prover types,
//! and the gather rule's soundness turns on whether an `unnest` element type *is* the column's type.
//! `int4[]` into a `bigint` column, or `varchar(10)[]` into a `varchar(20)` one, gives the two sides
//! different assignment coercions. So this keeps each column's type as Postgres names it, with
//! aliases resolved, and marks the two things the checker refuses outright: a type modifier (the
//! length in `varchar(10)`, the precision in `numeric(10,2)`) and an array type.

use std::collections::HashMap;

use sqlparser::ast::{DataType, Expr, Ident, ObjectName, Statement, Value};

/// A column type, canonical enough that two spellings of one Postgres type compare equal.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct TypeKey {
    /// Base type with aliases resolved, e.g. `int4` for `INT`, `INTEGER` and `SERIAL`.
    pub name: String,
    /// Written with a modifier, e.g. `varchar(10)`. Allowed on a *column*: both sides then apply the
    /// same assignment coercion from the base type (the length check, the rounding). Refused on a
    /// *cast*: an explicit cast to `varchar(10)` truncates, where assignment raises.
    pub modded: bool,
    /// An array type. Refused as a column: `unnest` flattens every dimension.
    pub array: bool,
}

impl TypeKey {
    pub fn plain(&self) -> bool {
        !self.modded && !self.array
    }
}

/// Canonicalise a type as sqlparser renders it.
pub fn type_key(dt: &DataType) -> TypeKey {
    let (inner, array) = match dt {
        DataType::Array(elem) => {
            use sqlparser::ast::ArrayElemTypeDef as E;
            match elem {
                // `T ARRAY` is Postgres's other spelling of `T[]`.
                E::SquareBracket(t, _) | E::AngleBracket(t) | E::Parenthesis(t) | E::Qualified(t, _) => {
                    (format!("{t}"), true)
                }
                E::None => ("".to_string(), true),
            }
        }
        other => (format!("{other}"), false),
    };
    let mut s = inner.trim().to_string();
    let mut array = array;
    while let Some(stripped) = s.strip_suffix("[]") {
        s = stripped.trim_end().to_string();
        array = true;
    }
    // A modifier is anything in parentheses after the name.
    let (mut base, modded) = match s.find('(') {
        Some(i) => (s[..i].trim().to_string(), true),
        None => (s, false),
    };
    // `pg_catalog` is where the built-in types live, so `pg_catalog.date` is `date`.
    if base.len() > 11 && base[..11].eq_ignore_ascii_case("pg_catalog.") {
        base = base[11..].to_string();
    }
    // A quoted name is exactly its contents (`"json"` is `json`); an unquoted one folds to lower case.
    let lower = match base.strip_prefix('"').and_then(|b| b.strip_suffix('"')) {
        Some(inner) if !inner.contains('"') => inner.to_string(),
        _ => base.to_lowercase(),
    };
    let name = match lower.as_str() {
        "int" | "integer" | "int4" | "serial" | "serial4" => "int4",
        "bigint" | "int8" | "bigserial" | "serial8" => "int8",
        "smallint" | "int2" | "smallserial" | "serial2" => "int2",
        "bool" | "boolean" => "bool",
        "real" | "float4" => "float4",
        "double precision" | "float8" | "double" => "float8",
        "numeric" | "decimal" => "numeric",
        "varchar" | "character varying" => "varchar",
        "timestamp" | "timestamp without time zone" => "timestamp",
        "timestamptz" | "timestamp with time zone" => "timestamptz",
        "time" | "time without time zone" => "time",
        "timetz" | "time with time zone" => "timetz",
        other => other,
    }
    .to_string();
    // `char`/`character` without a length is `char(1)`, and a bare `float` is `float8` only by
    // default precision: treat both as modified rather than guess.
    let modded = modded || matches!(name.as_str(), "char" | "character" | "bpchar" | "float");
    TypeKey { name, modded, array }
}

/// An identifier as Postgres resolves it: unquoted names fold to lower case, quoted ones do not.
pub fn fold(id: &Ident) -> String {
    match id.quote_style {
        None => id.value.to_lowercase(),
        Some(_) => id.value.clone(),
    }
}

/// The last part of a possibly qualified name, folded. `None` for a non-identifier part.
pub fn last_name(name: &ObjectName) -> Option<String> {
    name.0.last().and_then(|p| p.as_ident()).map(fold)
}

/// How a column the `INSERT` omits gets its value, as the witness model sees it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DefaultKind {
    /// No default, or `DEFAULT NULL`.
    Null,
    /// One value for every row of the statement: a constant, a statement clock (`now()`), or a
    /// default we do not recognise. Unrecognised defaults land here on purpose: it is the choice
    /// that makes rows collide most, so it can only withhold a witness, never invent one.
    Same,
    /// A new value per row: a sequence, an identity, `gen_random_uuid()`.
    Fresh,
}

/// The functions a `DEFAULT` of which gives each row a new value, [`DefaultKind::Fresh`].
///
/// A function that gives a new value per call is volatile, so each of these must also be on the
/// frontend's [`sqleq_frontend::VOLATILE_FUNCTIONS`], the one list of volatile functions;
/// [`default_kind`] checks both, and a test keeps every entry here on that list. Volatile is not
/// enough to be here: a clock (`clock_timestamp`), a sequence read (`currval`) or a function called
/// for its effect can give two rows the same value, so a default calling one stays
/// [`DefaultKind::Same`], the direction that can only withhold a witness.
const FRESH_DEFAULTS: &[&str] = &[
    "nextval",
    "gen_random_uuid",
    "uuid_generate_v1",
    "uuid_generate_v1mc",
    "uuid_generate_v4",
    "uuid_generate_v7",
    "uuidv4",
    "uuidv7",
    "random",
    "gen_random_bytes",
];

/// Classify a `DEFAULT` expression.
pub fn default_kind(e: &Expr) -> DefaultKind {
    match e {
        Expr::Nested(inner) => default_kind(inner),
        Expr::Value(v) if matches!(v.value, Value::Null) => DefaultKind::Null,
        Expr::Function(f) => {
            let name = last_name(&f.name).unwrap_or_default();
            if FRESH_DEFAULTS.contains(&name.as_str()) && sqleq_frontend::is_volatile(&name) {
                DefaultKind::Fresh
            } else {
                DefaultKind::Same
            }
        }
        // `'x'::text`, `nextval('s'::regclass)` is a Function above; a cast of anything else is
        // judged by what it casts.
        Expr::Cast { expr, .. } => match default_kind(expr) {
            DefaultKind::Null => DefaultKind::Null,
            k => k,
        },
        _ => DefaultKind::Same,
    }
}

/// What a column's default is, at the precision a `DEFAULT` cell needs: whether its value is fixed
/// by the schema, drawn from a sequence, or made by a generator the checker knows.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DefaultSource {
    /// No default, or a constant: a literal, possibly cast, or `NULL`.
    Constant,
    /// `nextval('s')`, a serial or an identity, by the sequence's name.
    Sequence(String),
    /// One of the recognised generators, such as `now()` or `gen_random_uuid()`.
    Generator,
    /// Anything else: an unrecognised function, or an expression around one.
    Other,
}

/// Classify a `DEFAULT` expression for a `DEFAULT` cell.
pub fn default_source(e: &Expr) -> DefaultSource {
    match e {
        Expr::Nested(inner) => default_source(inner),
        Expr::Value(_) | Expr::TypedString(_) => DefaultSource::Constant,
        Expr::UnaryOp { expr, .. } if matches!(expr.as_ref(), Expr::Value(_)) => DefaultSource::Constant,
        Expr::Cast { expr, .. } => default_source(expr),
        Expr::Function(f) => match crate::recognize::generator(f) {
            Ok(g) => match g.seq {
                Some(s) => DefaultSource::Sequence(s),
                None => DefaultSource::Generator,
            },
            Err(_) => DefaultSource::Other,
        },
        _ => DefaultSource::Other,
    }
}

/// What `e` does to sequences: the sequences whose `nextval` it calls (by string literal), and
/// whether it reads or sets sequence state any other way (`currval`, `lastval`, `setval`, or
/// `nextval` of something that is not a literal).
pub fn sequence_use(e: &Expr) -> (Vec<String>, bool) {
    let mut seqs = Vec::new();
    let mut state = false;
    let _ = sqlparser::ast::visit_expressions(e, |x| {
        if let Expr::Function(f) = x {
            match last_name(&f.name).as_deref() {
                Some("nextval") => match crate::recognize::generator(f) {
                    Ok(g) => seqs.extend(g.seq),
                    Err(_) => state = true,
                },
                Some("currval" | "lastval" | "setval") => state = true,
                _ => {}
            }
        }
        std::ops::ControlFlow::<()>::Continue(())
    });
    (seqs, state)
}

#[derive(Clone, Debug)]
pub struct Column {
    pub name: String,
    /// The type as the DDL writes it, e.g. `character varying(8)`. For the Postgres replay.
    pub raw_type: String,
    /// The `DEFAULT` expression as written, if any. For the Postgres replay.
    pub default_text: Option<String>,
    pub ty: TypeKey,
    pub nullable: bool,
    pub default: DefaultKind,
    /// `GENERATED ALWAYS` (an identity or a computed column): an explicit value is an error.
    pub always: bool,
    /// The default, as a `DEFAULT` cell sees it.
    pub source: DefaultSource,
    /// The sequences the default draws from. A serial or identity draws from `<table>_<column>_seq`.
    pub seqs: Vec<String>,
    /// The default reads or sets sequence state another way: `currval`, `lastval`, `setval`.
    pub seq_state: bool,
}

/// A uniqueness constraint or unique index.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Unique {
    /// The constraint's name, where `ON CONFLICT ON CONSTRAINT` could name it.
    pub name: Option<String>,
    pub cols: Vec<usize>,
    /// `NULLS NOT DISTINCT`
    pub nnd: bool,
    /// A partial index (`WHERE …`): treated as total when checking collisions, which only adds
    /// them, and never inferred as an arbiter.
    pub partial: bool,
    /// On expressions rather than bare columns: `cols` are the columns the expressions read.
    /// Never inferred as an arbiter.
    pub expr: bool,
}

/// The name Postgres gives an unnamed constraint: `t_pkey`, or `t_a_b_key` for `UNIQUE (a, b)`.
/// Only used to resolve `ON CONFLICT ON CONSTRAINT`; truncation at 63 bytes and the numeric suffix
/// Postgres adds on a clash are not reproduced, so such a name simply does not resolve.
fn default_name(table: &str, cols: &[&str], pk: bool) -> String {
    if pk {
        format!("{table}_pkey")
    } else {
        format!("{table}_{}_key", cols.join("_"))
    }
}

#[derive(Clone, Debug)]
pub struct Table {
    pub name: String,
    pub columns: Vec<Column>,
    pub uniques: Vec<Unique>,
    pub has_check: bool,
    pub has_fk: bool,
    /// Parsed only after the frontend's retry, which can drop a `NOT NULL`. Nullability is then
    /// not trustworthy in the direction a witness needs, so such a table gets none.
    pub retried: bool,
    /// The DDL also `ALTER`s this table. Constraints added that way (`ADD PRIMARY KEY`, `SET NOT
    /// NULL`) are not read, and a missed constraint is the direction that invents a witness, so
    /// such a table gets none.
    pub altered: bool,
    pub has_trigger: bool,
    /// Some DDL statement naming this table could not be read, so a constraint may be missing.
    pub unread: bool,
    /// The table has an `EXCLUDE` constraint, which the witness model does not know.
    pub has_exclude: bool,
}

impl Table {
    pub fn column(&self, name: &str) -> Option<usize> {
        self.columns.iter().position(|c| c.name == name)
    }

    /// Resolve an index column to the table columns it reads. `None` flags an expression.
    fn index_cols(&self, e: &Expr, fold_names: bool) -> (Vec<usize>, bool) {
        let ident = |id: &Ident| if fold_names { id.value.to_lowercase() } else { fold(id) };
        match e {
            Expr::Identifier(id) => (self.column(&ident(id)).into_iter().collect(), false),
            Expr::CompoundIdentifier(p) => {
                (p.last().and_then(|id| self.column(&ident(id))).into_iter().collect(), false)
            }
            other => {
                let mut cols = Vec::new();
                let _ = sqlparser::ast::visit_expressions(other, |x| {
                    if let Expr::Identifier(id) = x {
                        if let Some(i) = self.column(&ident(id)) {
                            if !cols.contains(&i) {
                                cols.push(i);
                            }
                        }
                    }
                    std::ops::ControlFlow::<()>::Continue(())
                });
                (cols, true)
            }
        }
    }
}

/// Tables keyed by their unqualified, folded name. A name that two `CREATE TABLE`s share (say in
/// two schemas) maps to `None`: which one an `INSERT` means is not decidable from the name alone.
#[derive(Default, Debug)]
pub struct Schema {
    tables: HashMap<String, Option<Table>>,
}

impl Schema {
    pub fn table(&self, name: &str) -> Result<&Table, String> {
        match self.tables.get(name) {
            Some(Some(t)) => Ok(t),
            Some(None) => Err(format!("table {name} is declared more than once")),
            None => Err(format!("table {name} is not declared")),
        }
    }

    fn add(&mut self, t: Table) {
        self.tables
            .entry(t.name.clone())
            .and_modify(|e| *e = None)
            .or_insert(Some(t));
    }

    /// Read every `CREATE TABLE` among `statements`. `retried` marks statements that only parsed
    /// after the frontend's retry, which quotes every column name: their names are folded
    /// regardless of quoting, as the frontend's own catalog does.
    pub fn from_statements<'a>(statements: impl IntoIterator<Item = (&'a Statement, bool)>) -> Schema {
        use sqlparser::ast::{ColumnOption as O, GeneratedAs, NullsDistinctOption, TableConstraint as C};
        let statements: Vec<(&Statement, bool)> = statements.into_iter().collect();
        // Domains first, whatever the order of the DDL: a column of a domain type takes the
        // domain's default unless it declares its own, and the domain's CHECK applies to it.
        let mut domains: HashMap<String, (Option<&Expr>, bool)> = HashMap::new();
        for (st, _) in &statements {
            if let Statement::CreateDomain(d) = st {
                if let Some(name) = last_name(&d.name) {
                    let check = d.constraints.iter().any(|c| matches!(c, C::Check(_)));
                    domains.insert(name, (d.default.as_ref(), check));
                }
            }
        }
        let mut s = Schema::default();
        let mut indexes = Vec::new();
        let mut altered = Vec::new();
        let mut triggered = Vec::new();
        for (st, retried) in statements {
            let ct = match st {
                Statement::CreateTable(ct) => ct,
                Statement::CreateIndex(ci) if ci.unique => {
                    indexes.push((ci, retried));
                    continue;
                }
                Statement::AlterTable(at) => {
                    altered.extend(last_name(&at.name));
                    continue;
                }
                Statement::CreateTrigger(tr) => {
                    triggered.extend(last_name(&tr.table_name));
                    continue;
                }
                _ => continue,
            };
            let Some(name) = last_name(&ct.name) else { continue };
            let mut t = Table {
                name: name.clone(),
                columns: Vec::new(),
                uniques: Vec::new(),
                has_check: false,
                has_fk: false,
                retried,
                altered: false,
                has_trigger: false,
                unread: false,
                has_exclude: false,
            };
            for c in &ct.columns {
                let ty = type_key(&c.data_type);
                let serial = {
                    let raw = format!("{}", c.data_type).to_lowercase();
                    matches!(raw.as_str(), "serial" | "serial2" | "serial4" | "serial8" | "smallserial" | "bigserial")
                };
                let col_name = if retried { c.name.value.to_lowercase() } else { fold(&c.name) };
                // The sequence a serial or identity column creates, as Postgres names it (names
                // over 63 bytes, which Postgres truncates, are not reproduced).
                let implicit_seq = format!("{name}_{col_name}_seq");
                let mut col = Column {
                    name: col_name,
                    raw_type: format!("{}", c.data_type),
                    default_text: None,
                    ty,
                    nullable: !serial,
                    default: if serial { DefaultKind::Fresh } else { DefaultKind::Null },
                    always: false,
                    source: if serial { DefaultSource::Sequence(implicit_seq.clone()) } else { DefaultSource::Constant },
                    seqs: if serial { vec![implicit_seq.clone()] } else { Vec::new() },
                    seq_state: false,
                };
                if let Some(&(default, check)) = domains.get(unqualified(&col.ty.name)).filter(|_| !col.ty.array) {
                    if let Some(e) = default {
                        col.default = default_kind(e);
                        col.default_text = Some(e.to_string());
                        col.source = default_source(e);
                        (col.seqs, col.seq_state) = sequence_use(e);
                    }
                    t.has_check |= check;
                }
                let idx = t.columns.len();
                for o in &c.options {
                    match &o.option {
                        O::NotNull => col.nullable = false,
                        O::Default(e) => {
                            col.default = default_kind(e);
                            col.default_text = Some(e.to_string());
                            col.source = default_source(e);
                            (col.seqs, col.seq_state) = sequence_use(e);
                        }
                        O::PrimaryKey(pk) => {
                            col.nullable = false;
                            let name = pk.name.as_ref().or(o.name.as_ref()).map(fold)
                                .or_else(|| Some(default_name(&name, &[], true)));
                            t.uniques.push(Unique { name, cols: vec![idx], nnd: false, partial: false, expr: false });
                        }
                        O::Unique(uc) => {
                            let name = uc.name.as_ref().or(o.name.as_ref()).map(fold)
                                .or_else(|| Some(default_name(&name, &[&col.name], false)));
                            let nnd = matches!(uc.nulls_distinct, NullsDistinctOption::NotDistinct);
                            t.uniques.push(Unique { name, cols: vec![idx], nnd, partial: false, expr: false });
                        }
                        O::Check(_) => t.has_check = true,
                        O::ForeignKey(_) => t.has_fk = true,
                        O::Generated { generated_as, generation_expr, .. } => {
                            if generation_expr.is_some() {
                                col.always = true;
                                col.default = DefaultKind::Same;
                                col.source = DefaultSource::Other;
                            } else {
                                col.default = DefaultKind::Fresh;
                                col.nullable = false;
                                col.always = matches!(generated_as, GeneratedAs::Always);
                                col.source = DefaultSource::Sequence(implicit_seq.clone());
                                col.seqs = vec![implicit_seq.clone()];
                            }
                        }
                        O::Identity(_) => {
                            col.default = DefaultKind::Fresh;
                            col.source = DefaultSource::Sequence(implicit_seq.clone());
                            col.seqs = vec![implicit_seq.clone()];
                        }
                        _ => {}
                    }
                }
                t.columns.push(col);
            }
            for con in &ct.constraints {
                let (name, cols, nnd, pk) = match con {
                    C::Unique(uc) => (uc.name.as_ref(), &uc.columns, matches!(uc.nulls_distinct, NullsDistinctOption::NotDistinct), false),
                    C::PrimaryKey(pk) => (pk.name.as_ref(), &pk.columns, false, true),
                    C::Check(_) => {
                        t.has_check = true;
                        continue;
                    }
                    C::ForeignKey(_) => {
                        t.has_fk = true;
                        continue;
                    }
                    // An exclusion constraint can reject a row, and decide a target-less
                    // `ON CONFLICT DO NOTHING`, much as a unique key does. It is not modelled.
                    C::Exclude(_) => {
                        t.has_exclude = true;
                        continue;
                    }
                    _ => continue,
                };
                let mut set = Vec::new();
                let mut expr = false;
                for ic in cols {
                    let (cs, e) = t.index_cols(&ic.column.expr, retried);
                    expr |= e;
                    set.extend(cs);
                }
                if set.is_empty() {
                    continue;
                }
                if pk {
                    for &i in &set {
                        t.columns[i].nullable = false;
                    }
                }
                let names: Vec<&str> = set.iter().map(|&i| t.columns[i].name.as_str()).collect();
                let name = name.map(fold).or_else(|| Some(default_name(&t.name, &names, pk)));
                t.uniques.push(Unique { name, cols: set, nnd, partial: false, expr });
            }
            s.add(t);
        }
        for (names, flag) in [(altered, 0), (triggered, 1)] {
            for n in names {
                if let Some(Some(t)) = s.tables.get_mut(&n) {
                    if flag == 0 {
                        t.altered = true;
                    } else {
                        t.has_trigger = true;
                    }
                }
            }
        }
        for (ci, retried) in indexes {
            let Some(tname) = last_name(&ci.table_name) else { continue };
            let Some(Some(t)) = s.tables.get_mut(&tname) else { continue };
            let mut set = Vec::new();
            let mut expr = false;
            for ic in &ci.columns {
                let (cs, e) = t.index_cols(&ic.column.expr, retried);
                expr |= e;
                set.extend(cs);
            }
            if set.is_empty() {
                continue;
            }
            t.uniques.push(Unique {
                // An index is not a constraint: `ON CONFLICT ON CONSTRAINT` cannot name it.
                name: None,
                cols: set,
                nnd: ci.nulls_distinct == Some(false),
                partial: ci.predicate.is_some(),
                expr,
            });
        }
        s
    }

    /// Read raw Postgres DDL, one statement at a time, as the frontend's schema reader does.
    ///
    /// A statement neither sqlparser nor the frontend's retry can read might declare a constraint,
    /// and a missed constraint is the direction that invents a witness. So a table named by any
    /// such statement is marked [`Table::unread`] and gets no witness. One common case is repaired
    /// first: pg_dump's `CREATE [UNIQUE] INDEX … ON ONLY t` (for a partitioned table), which
    /// sqlparser does not accept, is read as the same index without `ONLY`.
    pub fn from_ddl(raw: &str) -> Schema {
        let (mut statements, rejected) = sqleq_frontend::pgddl::parse_statements_reporting(raw);
        let mut unread = Vec::new();
        for r in rejected {
            match repair_on_only(&r.statement) {
                Some(st) => statements.extend(st.into_iter().map(|s| (s, false))),
                None => unread.push(r.statement),
            }
        }
        let mut s = Schema::from_statements(statements.iter().map(|(s, r)| (s, *r)));
        // A domain sqlparser cannot read (it has no domain-level `NOT NULL`, for one) constrains
        // every column of its type, and those statements never name a table.
        let domains: Vec<String> = unread.iter().filter_map(|u| unread_domain(u)).collect();
        for t in s.tables.values_mut().flatten() {
            t.unread = unread.iter().any(|u| mentions(u, &t.name))
                || t.columns.iter().any(|c| domains.iter().any(|d| d == unqualified(&c.ty.name)));
        }
        s
    }
}

/// A type name without its schema: domains are keyed by their bare name, as tables are.
fn unqualified(name: &str) -> &str {
    name.rsplit('.').next().unwrap_or(name)
}

/// The domain an unreadable `CREATE DOMAIN name …` declares, folded as `type_key` folds a type name.
fn unread_domain(stmt: &str) -> Option<String> {
    let mut words = stmt.split_whitespace();
    let (Some(c), Some(d), Some(name)) = (words.next(), words.next(), words.next()) else { return None };
    if !(c.eq_ignore_ascii_case("create") && d.eq_ignore_ascii_case("domain")) {
        return None;
    }
    let last = name.rsplit('.').next().unwrap_or(name);
    Some(match last.strip_prefix('"').and_then(|n| n.strip_suffix('"')) {
        Some(q) => q.to_string(),
        None => last.to_lowercase(),
    })
}

/// `CREATE [UNIQUE] INDEX … ON ONLY t …`, parsed as the same statement without `ONLY`.
fn repair_on_only(stmt: &str) -> Option<Vec<Statement>> {
    let up = stmt.to_ascii_uppercase();
    let head = up.trim_start();
    if !(head.starts_with("CREATE INDEX") || head.starts_with("CREATE UNIQUE INDEX")) {
        return None;
    }
    let at = up.find(" ON ONLY ")?;
    let fixed = format!("{}{}", &stmt[..at + 4], &stmt[at + 9..]);
    sqlparser::parser::Parser::parse_sql(&sqlparser::dialect::PostgreSqlDialect {}, &fixed).ok()
}

/// Whether `stmt` names the table `name` as a whole word, ignoring case and quotes. Generous on
/// purpose: a false match only withholds a witness.
fn mentions(stmt: &str, name: &str) -> bool {
    let hay = stmt.to_lowercase().replace('"', "");
    let needle = name.to_lowercase();
    hay.match_indices(&needle).any(|(i, _)| {
        let before = hay[..i].chars().next_back();
        let after = hay[i + needle.len()..].chars().next();
        let word = |c: Option<char>| c.is_some_and(|c| c.is_alphanumeric() || c == '_');
        !word(before) && !word(after)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqlparser::dialect::PostgreSqlDialect;
    use sqlparser::parser::Parser;

    fn key(sql_type: &str) -> TypeKey {
        let sql = format!("CREATE TABLE t (c {sql_type})");
        let Statement::CreateTable(ct) = &Parser::parse_sql(&PostgreSqlDialect {}, &sql).unwrap()[0] else {
            unreachable!()
        };
        type_key(&ct.columns[0].data_type)
    }

    #[test]
    fn aliases_resolve_to_one_name() {
        for t in ["int", "INTEGER", "int4", "serial"] {
            assert_eq!(key(t), TypeKey { name: "int4".into(), modded: false, array: false }, "{t}");
        }
        assert_eq!(key("bigserial").name, "int8");
        assert_eq!(key("character varying").name, "varchar");
        assert_eq!(key("timestamp with time zone").name, "timestamptz");
        assert_eq!(key("TIMESTAMP").name, "timestamp");
        assert_eq!(key("pg_catalog.date").name, "date");
        assert_eq!(key("\"json\"").name, "json");
        assert_eq!(key("\"MyEnum\"").name, "MyEnum", "a quoted name keeps its case");
    }

    #[test]
    fn modifiers_and_arrays_are_marked() {
        assert!(key("varchar(10)").modded);
        assert!(key("numeric(10,2)").modded);
        assert!(key("char").modded, "bare char is char(1)");
        assert!(key("int[]").array);
        assert!(!key("text").modded && !key("text").array);
    }

    #[test]
    fn folding_follows_postgres() {
        let s = Schema::from_ddl(r#"CREATE TABLE "Foo" (a int, "B" text NOT NULL); CREATE TABLE bar (x int);"#);
        let foo = s.table("Foo").unwrap();
        assert_eq!(foo.column("a"), Some(0));
        assert_eq!(foo.column("B"), Some(1));
        assert!(!foo.columns[1].nullable);
        assert!(s.table("foo").is_err(), "a quoted name does not fold");
        assert!(s.table("bar").is_ok());
    }

    #[test]
    fn defaults_are_classified_for_the_witness() {
        let s = Schema::from_ddl(
            "CREATE TABLE t (
               a serial, b bigint GENERATED ALWAYS AS IDENTITY, c int GENERATED BY DEFAULT AS IDENTITY,
               d timestamptz DEFAULT now(), e uuid DEFAULT gen_random_uuid(), f text DEFAULT NULL,
               g text DEFAULT 'x'::text, h int DEFAULT nextval('s'::regclass), i int,
               j int GENERATED ALWAYS AS (i + 1) STORED, k text NOT NULL DEFAULT lower('X'));",
        );
        let t = s.table("t").unwrap();
        let k = |n: &str| t.columns[t.column(n).unwrap()].clone();
        assert_eq!((k("a").default, k("a").nullable, k("a").always), (DefaultKind::Fresh, false, false));
        assert_eq!((k("b").default, k("b").always), (DefaultKind::Fresh, true));
        assert_eq!((k("c").default, k("c").always), (DefaultKind::Fresh, false));
        assert_eq!(k("d").default, DefaultKind::Same, "a statement clock is one value per statement");
        assert_eq!(k("e").default, DefaultKind::Fresh);
        assert_eq!(k("f").default, DefaultKind::Null);
        assert_eq!(k("g").default, DefaultKind::Same);
        assert_eq!(k("h").default, DefaultKind::Fresh);
        assert_eq!(k("i").default, DefaultKind::Null);
        assert!(k("j").always, "a computed column takes no explicit value");
        assert_eq!((k("k").default, k("k").nullable), (DefaultKind::Same, false), "an unknown function is Same");
    }

    #[test]
    fn a_domain_gives_its_columns_its_default_and_check() {
        let s = Schema::from_ddl(
            "CREATE TABLE t (a email, b email DEFAULT 'own', c public.email, d int, e email[]);
             CREATE DOMAIN email AS text DEFAULT 'nobody' CHECK (VALUE <> '');
             CREATE TABLE plain (a int);",
        );
        let t = s.table("t").unwrap();
        let k = |n: &str| t.columns[t.column(n).unwrap()].clone();
        assert_eq!(k("a").default_text.as_deref(), Some("'nobody'"), "the domain's default, declared after the table");
        assert_eq!((k("a").default, k("a").source), (DefaultKind::Same, DefaultSource::Constant));
        assert_eq!(k("b").default_text.as_deref(), Some("'own'"), "a column's own default wins");
        assert_eq!(k("c").default_text.as_deref(), Some("'nobody'"), "a qualified domain name");
        assert_eq!(k("e").default_text, None, "an array of the domain is not the domain");
        assert!(t.has_check, "the domain's CHECK applies to the table");
        assert!(!s.table("plain").unwrap().has_check);
    }

    #[test]
    fn an_unreadable_domain_marks_the_tables_that_use_it() {
        // sqlparser has no domain-level NOT NULL, so this domain is not read, and the NOT NULL
        // it puts on `t.a` would be missed.
        let s = Schema::from_ddl(
            "CREATE DOMAIN pos AS int NOT NULL;
             CREATE TABLE t (a pos, b text);
             CREATE TABLE u (a int);",
        );
        assert!(s.table("t").unwrap().unread);
        assert!(!s.table("u").unwrap().unread);
    }

    #[test]
    fn defaults_are_classified_for_a_default_cell() {
        let s = Schema::from_ddl(
            "CREATE TABLE t (
               a serial, b bigint GENERATED ALWAYS AS IDENTITY, d timestamptz DEFAULT now(),
               f text DEFAULT NULL, g text DEFAULT 'x'::text, m int DEFAULT -1,
               h int DEFAULT nextval('public.\"S\"'::regclass), i int, j int GENERATED ALWAYS AS (i + 1) STORED,
               k text DEFAULT lower('X'), l bigint DEFAULT currval('t_a_seq'),
               n text DEFAULT 'p' || nextval('s2'));",
        );
        let t = s.table("t").unwrap();
        let k = |n: &str| t.columns[t.column(n).unwrap()].clone();
        let seq = |s: &str| DefaultSource::Sequence(s.to_string());
        assert_eq!((k("a").source, k("a").seqs), (seq("t_a_seq"), vec!["t_a_seq".to_string()]));
        assert_eq!(k("b").source, seq("t_b_seq"), "an identity draws from its own sequence");
        assert_eq!(k("d").source, DefaultSource::Generator);
        for c in ["f", "g", "m", "i"] {
            assert_eq!(k(c).source, DefaultSource::Constant, "{c}");
        }
        assert_eq!(k("h").source, seq("S"), "a quoted, qualified sequence name");
        assert_eq!(k("j").source, DefaultSource::Other);
        assert_eq!(k("k").source, DefaultSource::Other, "an unknown function");
        assert_eq!((k("l").source.clone(), k("l").seq_state), (DefaultSource::Other, true));
        assert_eq!((k("n").source.clone(), k("n").seqs, k("n").seq_state), (DefaultSource::Other, vec!["s2".to_string()], false));
    }

    #[test]
    fn every_uniqueness_spelling_is_read() {
        let s = Schema::from_ddl(
            "CREATE TABLE t (id int PRIMARY KEY, a text UNIQUE, b int, c int, email text,
               CONSTRAINT t_bc UNIQUE NULLS NOT DISTINCT (b, c), CHECK (b > 0),
               FOREIGN KEY (c) REFERENCES p (id));
             CREATE UNIQUE INDEX t_live ON t (a) WHERE b IS NULL;
             CREATE UNIQUE INDEX t_email ON t (lower(email));
             CREATE INDEX t_plain ON t (b);",
        );
        let t = s.table("t").unwrap();
        assert!(!t.columns[0].nullable, "a primary key is NOT NULL");
        let u = &t.uniques;
        assert_eq!(u.len(), 5, "{u:?}");
        assert_eq!(u[0].cols, [0]);
        assert_eq!(u[1].cols, [1]);
        assert_eq!((u[2].name.as_deref(), u[2].cols.clone(), u[2].nnd), (Some("t_bc"), vec![2, 3], true));
        assert!(u[3].partial && !u[3].expr && u[3].name.is_none());
        assert!(u[4].expr && u[4].cols == [4]);
        assert!(t.has_check && t.has_fk && !t.retried && !t.altered);
    }

    #[test]
    fn an_index_on_only_is_read_and_an_unreadable_statement_marks_its_table() {
        let s = Schema::from_ddl(
            "CREATE TABLE t (a int, b text); CREATE UNIQUE INDEX t_ab ON ONLY public.t USING btree (a, b);
             CREATE TABLE u (x int); CREATE UNIQUE INDEX u_x ON u USING some_new_syntax !! (x);
             CREATE TABLE v (y int);",
        );
        let t = s.table("t").unwrap();
        assert_eq!(t.uniques.len(), 1, "ON ONLY is read");
        assert!(!t.unread);
        assert!(s.table("u").unwrap().unread, "an unreadable index on u");
        assert!(!s.table("v").unwrap().unread);
    }

    #[test]
    fn the_t_array_spelling_is_an_array() {
        assert!(key("int ARRAY").array);
        assert_eq!(key("int ARRAY").name, "int4");
        assert!(key("text ARRAY[3]").array);
    }

    #[test]
    fn an_exclusion_constraint_is_marked() {
        let s = Schema::from_ddl(
            "CREATE TABLE r (room int, during tsrange, EXCLUDE USING gist (room WITH =, during WITH &&));
             CREATE TABLE q (x int);",
        );
        assert!(s.table("r").unwrap().has_exclude, "EXCLUDE parses in sqlparser 0.63 and must be seen");
        assert!(!s.table("q").unwrap().has_exclude);
    }

    #[test]
    fn an_altered_or_triggered_table_is_marked() {
        let s = Schema::from_ddl(
            "CREATE TABLE a (x int); ALTER TABLE a ADD PRIMARY KEY (x);
             CREATE TABLE b (y int);
             CREATE TRIGGER tg BEFORE INSERT ON b FOR EACH ROW EXECUTE FUNCTION f();",
        );
        assert!(s.table("a").unwrap().altered);
        assert!(!s.table("b").unwrap().altered && s.table("b").unwrap().has_trigger);
    }

    #[test]
    fn a_name_declared_twice_is_ambiguous() {
        let s = Schema::from_ddl("CREATE TABLE a.t (x int); CREATE TABLE b.t (y int);");
        assert!(s.table("t").unwrap_err().contains("more than once"));
    }
}

/// [`FRESH_DEFAULTS`] against the frontend's list of volatile functions.
#[cfg(test)]
mod fresh_defaults {
    use super::*;

    #[test]
    fn every_fresh_default_is_on_the_frontends_volatile_list() {
        for f in FRESH_DEFAULTS {
            assert!(sqleq_frontend::is_volatile(f), "{f} is not on sqleq_frontend::VOLATILE_FUNCTIONS");
        }
    }

    #[test]
    fn a_volatile_default_that_can_repeat_is_same() {
        let s = Schema::from_ddl(
            "CREATE TABLE t (a timestamptz DEFAULT clock_timestamp(), b bigint DEFAULT currval('s'));",
        );
        let t = s.table("t").unwrap();
        for c in ["a", "b"] {
            assert_eq!(t.columns[t.column(c).unwrap()].default, DefaultKind::Same, "{c}");
        }
    }
}
