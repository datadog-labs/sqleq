// Unless explicitly stated otherwise all files in this repository are licensed under the
// Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/).
// Copyright 2026-Present Datadog, Inc.

//! Pairs of the gather rule's shape, and what the rule binds their parameters to.
//!
//! The pair is an `INSERT … VALUES` against an `INSERT … SELECT * FROM unnest(…)`:
//!
//! ```sql
//! INSERT INTO t (a, b) VALUES ($1, $2), ($3, $4);
//! INSERT INTO t (a, b) SELECT * FROM unnest($1::int[], $2::text[]);
//! ```
//!
//! On the `VALUES` side `$1` is a value and on the `unnest` side an array, so binding `$N` alike on
//! both sides compares nothing the caller paired. The gather rule, which the Lean axis proves under
//! (`docs/LEAN.md`), relates them instead: the `unnest` side's array `$j` is column `j` of the
//! `VALUES` rows, in row order. Here `$1 = [$1, $3]` and `$2 = [$2, $4]`, in terms of the `VALUES`
//! side's parameters. [`recognize`] reads a pair into that binding, and the Postgres engine runs it
//! under the rule ([`crate::pg`]).
//!
//! Only what the binding needs is read: which parameter fills each cell, and which array each
//! `unnest` argument is. Everything else the statements say -- a conflict clause, `RETURNING`, the
//! column types -- is Postgres's to run, and what the two sides do is compared as for any other
//! pair.

use sqlparser::ast::{
    CastKind, DataType, Expr, Ident, Insert, ObjectName, Query, SelectItem, SetExpr, Statement,
    TableFactor, TableObject, Value,
};
use sqlparser::dialect::PostgreSqlDialect;
use sqlparser::parser::Parser;

/// A `VALUES` cell the binding can gather: a parameter, cast or not, or `NULL`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Cell {
    /// `$n`, and the type it is cast to, as written, when it is.
    Param(u32, Option<String>),
    Null,
    /// A cell Postgres evaluates itself: a generator such as `DEFAULT` or `nextval(…)`, or an
    /// expression. The rule has no parameter to take its column's array from, so only
    /// [`recognize_for_probe`] reads a pair with one.
    Opaque,
}

/// [`recognize`]'s refusal for a `VALUES` cell that generates its own value.
pub const GENERATED: &str = "generated cells";
/// [`recognize`]'s refusal for any other `VALUES` cell that is not a parameter or `NULL`.
pub const NOT_A_PARAM: &str = "a VALUES cell that is not a parameter or NULL";

/// A pair read under the gather rule.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Gather {
    /// Whether the `VALUES` side is the pair's first statement.
    pub values_first: bool,
    /// The target columns, folded, in the order both sides list them.
    pub cols: Vec<String>,
    /// The `VALUES` rows, each `cols.len()` cells wide.
    pub rows: Vec<Vec<Cell>>,
    /// The `unnest` arguments in order, one per target column: the parameter each one is, and its
    /// element type as written.
    pub args: Vec<(u32, String)>,
}

impl Gather {
    /// The `VALUES` column the `unnest` side's `$p` is gathered from.
    pub fn column_of(p: u32) -> usize {
        p as usize - 1
    }

    /// The column a `VALUES` side's parameter first fills.
    pub fn column_of_param(&self, n: u32) -> Option<usize> {
        self.rows.iter().find_map(|r| {
            r.iter()
                .position(|c| matches!(c, Cell::Param(m, _) if *m == n))
        })
    }

    /// The type texts that meet the values of `VALUES` column `c` on their way into the table: the
    /// casts its cells write, and the element type of every `unnest` argument gathered from it.
    pub fn types_meeting(&self, c: usize) -> Vec<String> {
        let mut out: Vec<String> = self
            .rows
            .iter()
            .filter_map(|r| match r.get(c) {
                Some(Cell::Param(_, Some(t))) => Some(t.clone()),
                _ => None,
            })
            .collect();
        out.extend(
            self.args
                .iter()
                .filter(|(p, _)| Self::column_of(*p) == c)
                .map(|(_, t)| t.clone()),
        );
        out
    }

    /// The target columns that values of `VALUES` column `c` are inserted into: column `c` on the
    /// `VALUES` side, and on the `unnest` side the column of every argument gathered from it.
    pub fn columns_fed(&self, c: usize) -> Vec<&str> {
        let mut out = vec![self.cols[c].as_str()];
        for (i, (p, _)) in self.args.iter().enumerate() {
            if Self::column_of(*p) == c && !out.contains(&self.cols[i].as_str()) {
                out.push(self.cols[i].as_str());
            }
        }
        out
    }
}

/// A name as Postgres resolves it: folded to lower case unless double-quoted.
fn fold(id: &Ident) -> String {
    match id.quote_style {
        Some('"') => id.value.clone(),
        _ => id.value.to_ascii_lowercase(),
    }
}

fn fold_name(name: &ObjectName) -> Option<Vec<String>> {
    name.0.iter().map(|p| p.as_ident().map(fold)).collect()
}

fn strip(e: &Expr) -> &Expr {
    match e {
        Expr::Nested(inner) => strip(inner),
        other => other,
    }
}

fn placeholder(e: &Expr) -> Option<u32> {
    match strip(e) {
        Expr::Value(v) => match &v.value {
            Value::Placeholder(s) => s.strip_prefix('$')?.parse().ok(),
            _ => None,
        },
        _ => None,
    }
}

/// `CAST(x AS t)` and `x::t`; `TRY_CAST` and `SAFE_CAST` are not Postgres casts.
fn postgres_cast(kind: &CastKind) -> bool {
    matches!(kind, CastKind::Cast | CastKind::DoubleColon)
}

/// What one `VALUES` cell is: gatherable, or the reason it is not.
fn cell(e: &Expr) -> Result<Cell, String> {
    if let Some(n) = placeholder(e) {
        return Ok(Cell::Param(n, None));
    }
    match strip(e) {
        Expr::Value(v) if matches!(v.value, Value::Null) => Ok(Cell::Null),
        Expr::Cast {
            kind,
            expr,
            data_type,
            format: None,
            ..
        } if postgres_cast(kind) => match placeholder(expr) {
            Some(n) => Ok(Cell::Param(n, Some(data_type.to_string()))),
            None => Err(NOT_A_PARAM.to_string()),
        },
        Expr::Identifier(id)
            if id.quote_style.is_none() && id.value.eq_ignore_ascii_case("default") =>
        {
            Err(GENERATED.to_string())
        }
        Expr::Function(_) => Err(GENERATED.to_string()),
        _ => Err(NOT_A_PARAM.to_string()),
    }
}

/// An `unnest` argument `$p::T[]`: the parameter and the element type `T`.
fn unnest_arg(e: &Expr) -> Option<(u32, String)> {
    let Expr::Cast {
        kind,
        expr,
        data_type: DataType::Array(elem),
        format: None,
        ..
    } = strip(e)
    else {
        return None;
    };
    if !postgres_cast(kind) {
        return None;
    }
    use sqlparser::ast::ArrayElemTypeDef as E;
    let elem = match elem {
        E::SquareBracket(t, _) | E::AngleBracket(t) | E::Parenthesis(t) => t,
        E::None | E::Qualified(..) => return None,
    };
    Some((placeholder(expr)?, elem.to_string()))
}

struct Side<'a> {
    target: Vec<String>,
    cols: Vec<String>,
    source: &'a Query,
}

fn side(stmt: &Statement) -> Option<Side<'_>> {
    let Statement::Insert(Insert {
        table: TableObject::TableName(name),
        columns,
        source: Some(source),
        ..
    }) = stmt
    else {
        return None;
    };
    let cols = columns
        .iter()
        .map(|c| match c.0.as_slice() {
            [part] => part.as_ident().map(fold),
            _ => None,
        })
        .collect::<Option<Vec<_>>>()?;
    if cols.is_empty() {
        return None;
    }
    Some(Side {
        target: fold_name(name)?,
        cols,
        source,
    })
}

/// The rows of a source that is `VALUES` and nothing else.
fn values_rows(q: &Query) -> Option<Vec<&Vec<Expr>>> {
    let SetExpr::Values(v) = q.body.as_ref() else {
        return None;
    };
    // A clause around the list (`ORDER BY`, `LIMIT`, `WITH`) renders, so this is the list alone.
    if q.to_string() != q.body.to_string() {
        return None;
    }
    Some(v.rows.iter().map(|r| &r.content).collect())
}

/// The arguments of a source that is `SELECT * FROM unnest(…)` and nothing else.
fn unnest_args(q: &Query) -> Option<Vec<&Expr>> {
    let SetExpr::Select(sel) = q.body.as_ref() else {
        return None;
    };
    let [SelectItem::Wildcard(_)] = sel.projection.as_slice() else {
        return None;
    };
    let [twj] = sel.from.as_slice() else {
        return None;
    };
    // Every clause of the query and the select renders, so a source that renders as exactly this
    // is this and nothing more: no `WHERE`, join, `DISTINCT`, `ORDER BY` or `LIMIT`.
    if !twj.joins.is_empty() || q.to_string() != format!("SELECT * FROM {}", twj.relation) {
        return None;
    }
    match &twj.relation {
        TableFactor::UNNEST {
            array_exprs,
            with_offset: false,
            with_offset_alias: None,
            with_ordinality: false,
            ..
        } => Some(array_exprs.iter().collect()),
        TableFactor::Function {
            lateral: false,
            name,
            args,
            with_ordinality: false,
            ..
        } if fold_name(name).as_deref() == Some(&["unnest".to_string()][..]) => {
            use sqlparser::ast::{FunctionArg, FunctionArgExpr};
            args.iter()
                .map(|a| match a {
                    FunctionArg::Unnamed(FunctionArgExpr::Expr(e)) => Some(e),
                    _ => None,
                })
                .collect()
        }
        _ => None,
    }
}

/// How many placeholders `sql` holds, wherever they are.
fn placeholder_count(sql: &str) -> Option<usize> {
    crate::patterns::placeholders(sql).ok().map(|p| p.len())
}

/// Read a pair under the gather rule.
///
/// `None` when the pair is not of the rule's shape, in either order: one `INSERT` whose source is a
/// `VALUES` list and nothing else, one whose source is `SELECT * FROM unnest(…)` of parameters cast
/// to arrays and nothing else, the same target and column list, every row and the `unnest` as wide
/// as the column list. Such a pair is tested as any other.
///
/// `Some(Err(why))` when it is of that shape but the rule cannot bind it: a `VALUES` cell that is
/// not a parameter or `NULL` (a generated cell has no parameter to gather; Lean's weaker claim for
/// those is not tested here), an `unnest` argument `$p` with no column `p`, or a parameter outside
/// the cells and the arguments, as in a conflict clause, where the two sides number them
/// differently.
pub fn recognize(a: &str, b: &str) -> Option<Result<Gather, String>> {
    recognize_with(a, b, false)
}

/// Whether a refusal of [`recognize`] is one the rejection probe reads past: a `VALUES` cell the rule
/// has no parameter for. The other refusals say the two sides do not line up at all.
pub fn probe_eligible(why: &str) -> bool {
    why == GENERATED || why == NOT_A_PARAM
}

/// The pair as [`recognize`] reads it, with a cell the rule cannot gather taken as [`Cell::Opaque`]
/// rather than refused, for the rejection probe in `crate::pg` alone. That probe asks only whether
/// the `unnest` side prepares as written. The arrays an opaque cell's column would be given are made
/// up, so nothing run under this binding is compared as an outcome of the pair.
pub fn recognize_for_probe(a: &str, b: &str) -> Option<Gather> {
    recognize_with(a, b, true)?.ok()
}

fn recognize_with(a: &str, b: &str, lenient: bool) -> Option<Result<Gather, String>> {
    let parse = |sql: &str| {
        let mut v = Parser::parse_sql(&PostgreSqlDialect {}, sql).ok()?;
        (v.len() == 1).then(|| v.remove(0))
    };
    let (sa, sb) = (parse(a)?, parse(b)?);
    let (xa, xb) = (side(&sa)?, side(&sb)?);
    let (values_first, vals, unn, vals_sql, unn_sql) =
        match (values_rows(xa.source), unnest_args(xb.source)) {
            (Some(_), Some(_)) => (true, &xa, &xb, a, b),
            _ => match (values_rows(xb.source), unnest_args(xa.source)) {
                (Some(_), Some(_)) => (false, &xb, &xa, b, a),
                _ => return None,
            },
        };
    if vals.target != unn.target || vals.cols != unn.cols {
        return None;
    }
    let k = vals.cols.len();
    let raw_rows = values_rows(vals.source)?;
    let raw_args = unnest_args(unn.source)?;
    if raw_rows.iter().any(|r| r.len() != k) || raw_args.len() != k {
        return None;
    }
    let args = raw_args
        .iter()
        .map(|e| unnest_arg(e))
        .collect::<Option<Vec<_>>>()?;
    let rows = match raw_rows
        .iter()
        .map(|r| {
            r.iter()
                .map(|e| match cell(e) {
                    Err(why) if lenient && probe_eligible(&why) => Ok(Cell::Opaque),
                    c => c,
                })
                .collect::<Result<Vec<_>, _>>()
        })
        .collect::<Result<Vec<_>, _>>()
    {
        Ok(rows) => rows,
        Err(why) => return Some(Err(why)),
    };
    if let Some((p, _)) = args.iter().find(|(p, _)| *p == 0 || *p as usize > k) {
        return Some(Err(format!(
            "unnest argument ${p} has no VALUES column {p}"
        )));
    }
    let in_cells = rows
        .iter()
        .flatten()
        .filter(|c| matches!(c, Cell::Param(..)))
        .count();
    // An opaque cell may hold parameters of its own (`nextval($1)`); the probe binds every parameter of
    // the `VALUES` side as drawn, so only a strict reading needs them all to be cells.
    if !lenient && placeholder_count(vals_sql) != Some(in_cells) {
        return Some(Err("a parameter outside the VALUES rows".to_string()));
    }
    if placeholder_count(unn_sql) != Some(args.len()) {
        return Some(Err("a parameter outside the unnest arguments".to_string()));
    }
    Some(Ok(Gather {
        values_first,
        cols: vals.cols.clone(),
        rows,
        args,
    }))
}

/// What a type text says about the values it holds, as far as the boundary values below need.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Kind {
    /// An integer type and its largest value.
    Int(i64),
    /// `numeric(p, s)`'s scale, when it has one.
    Numeric(Option<u32>),
    Float4,
    Float8,
    /// A string type and its length limit, when it has one.
    Str(Option<usize>),
    /// `timestamp` (without time zone).
    Ts,
    /// `timestamptz`.
    Tstz,
    Other,
}

/// Read a type as written (`VARCHAR(8)`, `int`) or as Postgres names it (`character varying(8)`,
/// `timestamp without time zone`). An array's element type is read for the array.
pub fn kind(ty: &str) -> Kind {
    let t = ty.trim().to_ascii_lowercase();
    let t = t.trim_end_matches("[]").trim();
    let t = t.strip_prefix("pg_catalog.").unwrap_or(t);
    let (base, mods) = match t.find('(') {
        Some(i) => (t[..i].trim(), &t[i..]),
        None => (t, ""),
    };
    // `timestamp(3) with time zone` puts its modifier in the middle.
    let rest = mods.find(')').map_or("", |i| mods[i + 1..].trim());
    let nums: Vec<i64> = mods
        .trim_start_matches('(')
        .split(')')
        .next()
        .unwrap_or("")
        .split(',')
        .filter_map(|n| n.trim().parse().ok())
        .collect();
    let base = if rest.is_empty() {
        base.to_string()
    } else {
        format!("{base} {rest}")
    };
    match base.as_str() {
        "smallint" | "int2" => Kind::Int(i16::MAX as i64),
        "int" | "integer" | "int4" => Kind::Int(i32::MAX as i64),
        "bigint" | "int8" => Kind::Int(i64::MAX),
        // `char` with no length is `char(1)`; `bpchar` with none has no limit.
        "char" | "character" => Kind::Str(Some(nums.first().map_or(1, |n| *n as usize))),
        "bpchar" | "varchar" | "character varying" => Kind::Str(nums.first().map(|n| *n as usize)),
        "text" | "citext" | "name" => Kind::Str(None),
        "numeric" | "decimal" => Kind::Numeric(match nums.as_slice() {
            [] => None,
            [_] => Some(0),
            [_, s, ..] => Some(*s as u32),
        }),
        "real" | "float4" => Kind::Float4,
        "float" => match nums.first() {
            Some(p) if *p <= 24 => Kind::Float4,
            _ => Kind::Float8,
        },
        "double precision" | "float8" => Kind::Float8,
        "timestamp" | "timestamp without time zone" => Kind::Ts,
        "timestamptz" | "timestamp with time zone" => Kind::Tstz,
        _ => Kind::Other,
    }
}

/// A value for `$n` that neither the generated tables nor another parameter of the trial is likely to
/// hold: `v`'s kind, moved out of the small domains the draws share with the tables. `ty` is the type
/// Postgres gives `$n`. The draws repeat a handful of values, so the rows of an `INSERT` meet the
/// rows already there, or each other, on a key, and both sides raise; on these values they need not.
/// A kind with no room to move (a boolean, a date, a timestamp) keeps its value.
pub fn fresh(v: &crate::gen::Val, n: u32, ty: &str) -> crate::gen::Val {
    use crate::gen::Val;
    match v {
        Val::Int(_) => Val::Int(100 + n as i64),
        Val::Dbl(_) => Val::Dbl(100.0 + n as f64),
        // Only a string type: a `Str` also carries `json` and `interval` values.
        Val::Str(s) if matches!(kind(ty), Kind::Str(_)) => Val::Str(format!("{s}{n}")),
        Val::Uuid(_) => Val::Uuid(format!("00000000-0000-4000-b000-{n:012}")),
        other => other.clone(),
    }
}

/// A local time that does not exist where clocks move forward (2021-03-14 in New York), and one
/// that happens twice where they move back: a `timestamp` read as a `timestamptz` and back again in
/// that zone comes back changed, or ambiguous.
pub const DST_GAP: &str = "2021-03-14 02:30:00";
pub const DST_OVERLAP: &str = "2021-11-07 01:30:00";
/// An instant with its offset written: a `timestamptz` input that a `timestamp` reads as the local
/// time it spells, dropping the offset.
pub const WITH_OFFSET: &str = "2021-06-01 12:00:00+00";
/// The zone the boundary trials run in.
pub const DST_ZONE: &str = "America/New_York";

/// Values worth trying for a `VALUES` parameter of type `param`, given the types `meet` its value on
/// either side's way into the table: where one of them holds fewer values than another, the first
/// value past the narrower one. The random domains are small on purpose and never reach these.
///
/// The second result says whether a time zone with daylight saving matters: a `timestamp` and a
/// `timestamptz` meet, so a value read as one and assigned as the other moves with the session's
/// zone, which is always UTC otherwise.
pub fn boundary_values(param: &str, meet: &[String]) -> (Vec<String>, bool) {
    let p = kind(param);
    let kinds: Vec<Kind> = meet.iter().map(|t| kind(t)).collect();
    let mut out: Vec<String> = Vec::new();
    let mut push = |v: String| {
        if !out.contains(&v) {
            out.push(v);
        }
    };
    let numeric_param = matches!(
        p,
        Kind::Int(_) | Kind::Numeric(_) | Kind::Float4 | Kind::Float8
    );
    let fits = |v: i128| match p {
        Kind::Int(max) => v.unsigned_abs() <= max as u128,
        _ => true,
    };
    for k in &kinds {
        match *k {
            Kind::Int(max) if numeric_param && max < i64::MAX => {
                for v in [max as i128 + 1, -(max as i128) - 2] {
                    if fits(v) {
                        push(v.to_string());
                    }
                }
            }
            Kind::Numeric(Some(s)) if numeric_param && !matches!(p, Kind::Int(_)) => {
                push(format!("1.{}", "5".repeat(s as usize + 1)));
            }
            Kind::Float4 if numeric_param && p != Kind::Float4 => {
                if fits(16_777_217) {
                    push("16777217".to_string());
                }
                if !matches!(p, Kind::Int(_)) {
                    push("0.1".to_string());
                }
            }
            Kind::Str(Some(n)) if matches!(p, Kind::Str(_)) => {
                push("x".repeat(n));
                push("x".repeat(n + 1));
            }
            _ => {}
        }
    }
    let zoned = (p == Kind::Ts && kinds.contains(&Kind::Tstz))
        || (p == Kind::Tstz && kinds.contains(&Kind::Ts));
    if zoned {
        push(DST_GAP.to_string());
        push(DST_OVERLAP.to_string());
        if p == Kind::Tstz {
            push(WITH_OFFSET.to_string());
        }
    }
    (out, zoned)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ok(a: &str, b: &str) -> Gather {
        match recognize(a, b) {
            Some(Ok(g)) => g,
            other => panic!("{a} / {b}: {other:?}"),
        }
    }

    #[test]
    fn a_gather_pair_is_read_in_either_order() {
        let v = "INSERT INTO t (a, b) VALUES ($1, $2), ($3, NULL)";
        let u = "INSERT INTO t (a, b) SELECT * FROM unnest($1::int[], $2::text[])";
        let g = ok(v, u);
        assert!(g.values_first);
        assert_eq!(g.cols, ["a", "b"]);
        assert_eq!(g.rows[1], [Cell::Param(3, None), Cell::Null]);
        assert_eq!(g.args, [(1, "INT".to_string()), (2, "TEXT".to_string())]);
        let g = ok(u, v);
        assert!(!g.values_first);
        assert_eq!(g.rows.len(), 2);
    }

    #[test]
    fn spellings_of_one_shape_are_one_shape() {
        for (v, u) in [
            (
                "insert into T (A) values (($1)), (CAST($2 AS bigint))",
                r#"INSERT INTO t ("a") SELECT * FROM UNNEST(CAST($1 AS INT8[])) AS u(x)"#,
            ),
            (
                "INSERT INTO s.t (a) VALUES ($1::int) ON CONFLICT (a) DO NOTHING RETURNING a",
                "INSERT INTO s.t (a) SELECT * FROM unnest($1::int[]) ON CONFLICT (a) DO NOTHING RETURNING a",
            ),
        ] {
            ok(v, u);
        }
        let g = ok(
            "insert into T (A) values (($1)), (CAST($2 AS bigint))",
            "INSERT INTO t (a) SELECT * FROM unnest($1::int8[])",
        );
        assert_eq!(g.rows[1], [Cell::Param(2, Some("BIGINT".to_string()))]);
    }

    #[test]
    fn the_argument_order_is_the_unnest_sides_own() {
        let g = ok(
            "INSERT INTO t (a, b) VALUES ($1, $2)",
            "INSERT INTO t (a, b) SELECT * FROM unnest($2::text[], $1::int[])",
        );
        assert_eq!(g.args.iter().map(|(p, _)| *p).collect::<Vec<_>>(), [2, 1]);
        assert_eq!(g.types_meeting(1), ["TEXT"]);
        assert_eq!(g.columns_fed(1), ["b", "a"]);
        assert_eq!(g.column_of_param(2), Some(1));
    }

    #[test]
    fn other_shapes_are_not_gather_pairs() {
        let u = "INSERT INTO t (a, b) SELECT * FROM unnest($1::int[], $2::text[])";
        for v in [
            // Another table, other columns, a narrow row, an unnest of the wrong width.
            "INSERT INTO s (a, b) VALUES ($1, $2)",
            "INSERT INTO t (b, a) VALUES ($1, $2)",
            "INSERT INTO t (a, b) VALUES ($1)",
            "INSERT INTO t (a, b) VALUES ($1, $2) ORDER BY 1",
            "INSERT INTO t (a, b) SELECT $1, $2",
            "UPDATE t SET a = $1",
            "INSERT INTO t VALUES ($1, $2)",
        ] {
            assert_eq!(recognize(v, u), None, "{v}");
        }
        let v = "INSERT INTO t (a, b) VALUES ($1, $2)";
        for u in [
            "INSERT INTO t (a, b) SELECT * FROM unnest($1::int[]) AS x(a), unnest($2::text[])",
            "INSERT INTO t (a, b) SELECT * FROM unnest($1::int[], $2::text[]) WHERE true",
            "INSERT INTO t (a, b) SELECT * FROM unnest($1::int[], $2::text[]) WITH ORDINALITY",
            "INSERT INTO t (a, b) SELECT DISTINCT * FROM unnest($1::int[], $2::text[])",
            "INSERT INTO t (a, b) SELECT * FROM unnest($1, $2)",
            "INSERT INTO t (a, b) SELECT * FROM unnest($1::int[])",
            "INSERT INTO t (a, b) SELECT * FROM app.unnest($1::int[], $2::text[])",
        ] {
            assert_eq!(recognize(v, u), None, "{u}");
        }
    }

    /// The probe's reading: a cell the rule cannot gather is opaque rather than refused, and so is
    /// a parameter inside one; [`recognize`] itself is unchanged. A refusal that says the two sides
    /// do not line up is not one the probe reads past.
    #[test]
    fn the_probe_reads_a_generated_or_expression_cell_as_opaque() {
        let u = "INSERT INTO t (a, b) SELECT * FROM unnest($1::int[], $2::text[])";
        for v in [
            "INSERT INTO t (a, b) VALUES (DEFAULT, $1), (DEFAULT, $2)",
            "INSERT INTO t (a, b) VALUES (nextval($1), $2)",
            "INSERT INTO t (a, b) VALUES (CAST($1::text AS int), $2)",
        ] {
            let why = match recognize(v, u) {
                Some(Err(why)) => why,
                other => panic!("{v}: {other:?}"),
            };
            assert!(probe_eligible(&why), "{v}: {why}");
            let g = recognize_for_probe(v, u).unwrap_or_else(|| panic!("{v}"));
            assert!(g.rows.iter().all(|r| r[0] == Cell::Opaque), "{v}: {:?}", g.rows);
            assert!(g.rows.iter().all(|r| matches!(r[1], Cell::Param(..))), "{v}");
        }
        // A conflict clause numbering a parameter apart is not a cell: the sides do not line up.
        assert!(!probe_eligible("a parameter outside the VALUES rows"));
        assert!(!probe_eligible("unnest argument $3 has no VALUES column 3"));
        // A pair the strict reading binds is the same pair to the probe.
        let v = "INSERT INTO t (a, b) VALUES ($1, $2)";
        assert_eq!(recognize_for_probe(v, u), recognize(v, u).and_then(Result::ok));
    }

    #[test]
    fn a_gather_shape_the_rule_cannot_bind_says_why() {
        let u = "INSERT INTO t (a, b) SELECT * FROM unnest($1::int[], $2::text[])";
        for (v, why) in [
            (
                "INSERT INTO t (a, b) VALUES (DEFAULT, $1)",
                "generated cells",
            ),
            ("INSERT INTO t (a, b) VALUES (now(), $1)", "generated cells"),
            (
                "INSERT INTO t (a, b) VALUES (1, $1)",
                "not a parameter or NULL",
            ),
            (
                "INSERT INTO t (a, b) VALUES ($1, $2) ON CONFLICT (a) DO UPDATE SET b = $3",
                "outside the VALUES rows",
            ),
        ] {
            match recognize(v, u) {
                Some(Err(e)) => assert!(e.contains(why), "{v}: {e}"),
                other => panic!("{v}: {other:?}"),
            }
        }
        let v = "INSERT INTO t (a, b) VALUES ($1, $2)";
        for (u, why) in [
            ("INSERT INTO t (a, b) SELECT * FROM unnest($1::int[], $3::text[])", "no VALUES column 3"),
            (
                "INSERT INTO t (a, b) SELECT * FROM unnest($1::int[], $2::text[]) ON CONFLICT (a) DO UPDATE SET b = $3",
                "outside the unnest arguments",
            ),
        ] {
            match recognize(v, u) {
                Some(Err(e)) => assert!(e.contains(why), "{u}: {e}"),
                other => panic!("{u}: {other:?}"),
            }
        }
    }

    #[test]
    fn kinds_read_either_spelling() {
        for (t, k) in [
            ("INT", Kind::Int(i32::MAX as i64)),
            ("integer", Kind::Int(i32::MAX as i64)),
            ("SMALLINT", Kind::Int(i16::MAX as i64)),
            ("bigint", Kind::Int(i64::MAX)),
            ("CHAR", Kind::Str(Some(1))),
            ("character(3)", Kind::Str(Some(3))),
            ("VARCHAR(8)", Kind::Str(Some(8))),
            ("character varying", Kind::Str(None)),
            ("bpchar", Kind::Str(None)),
            ("text[]", Kind::Str(None)),
            ("NUMERIC(10,2)", Kind::Numeric(Some(2))),
            ("numeric(5)", Kind::Numeric(Some(0))),
            ("numeric", Kind::Numeric(None)),
            ("REAL", Kind::Float4),
            ("float(53)", Kind::Float8),
            ("TIMESTAMP", Kind::Ts),
            ("timestamp(3) without time zone", Kind::Ts),
            ("TIMESTAMPTZ", Kind::Tstz),
            ("timestamp with time zone", Kind::Tstz),
            ("uuid", Kind::Other),
        ] {
            assert_eq!(kind(t), k, "{t}");
        }
    }

    #[test]
    fn fresh_values_are_apart_from_the_draws_and_each_other() {
        use crate::gen::Val;
        assert_eq!(fresh(&Val::Int(1), 3, "integer"), Val::Int(103));
        assert_ne!(fresh(&Val::Int(1), 3, "integer"), fresh(&Val::Int(1), 4, "integer"));
        assert_eq!(fresh(&Val::Str("a".into()), 7, "text"), Val::Str("a7".into()));
        // Not a string type: the text is a json or interval value, and stays one.
        assert_eq!(fresh(&Val::Str("1 day".into()), 7, "interval"), Val::Str("1 day".into()));
        assert_eq!(fresh(&Val::Bool(true), 7, "boolean"), Val::Bool(true));
        assert_eq!(fresh(&Val::Uuid("x".into()), 12, "uuid"), Val::Uuid("00000000-0000-4000-b000-000000000012".into()));
    }

    #[test]
    fn boundary_values_sit_just_past_the_narrower_type() {
        let s = |xs: &[&str]| xs.iter().map(|x| x.to_string()).collect::<Vec<_>>();
        // int4 elements gathered into a bigint column: one past int4 either way.
        assert_eq!(
            boundary_values("bigint", &s(&["INT"])),
            (s(&["2147483648", "-2147483649"]), false)
        );
        // An integer parameter cannot hold a value past its own type.
        assert_eq!(
            boundary_values("integer", &s(&["INT"])).0,
            Vec::<String>::new()
        );
        assert_eq!(
            boundary_values("integer", &s(&["SMALLINT"])).0,
            s(&["32768", "-32769"])
        );
        // A bare `char` is `char(1)`.
        assert_eq!(boundary_values("text", &s(&["CHAR"])).0, s(&["x", "xx"]));
        assert_eq!(
            boundary_values("character varying", &s(&["VARCHAR(8)"])).0,
            s(&["xxxxxxxx", "xxxxxxxxx"])
        );
        assert_eq!(
            boundary_values("numeric", &s(&["NUMERIC(10,1)"])).0,
            s(&["1.55"])
        );
        assert_eq!(
            boundary_values("double precision", &s(&["REAL"])).0,
            s(&["16777217", "0.1"])
        );
        assert_eq!(
            boundary_values("timestamp without time zone", &s(&["TIMESTAMPTZ"])),
            (s(&[DST_GAP, DST_OVERLAP]), true)
        );
        assert_eq!(
            boundary_values("timestamp with time zone", &s(&["timestamp"])),
            (s(&[DST_GAP, DST_OVERLAP, WITH_OFFSET]), true)
        );
        // Types that hold the same values call for nothing.
        assert_eq!(
            boundary_values("text", &s(&["TEXT", "uuid"])),
            (Vec::new(), false)
        );
        assert_eq!(
            boundary_values("timestamp without time zone", &s(&["TIMESTAMP"])),
            (Vec::new(), false)
        );
    }
}
