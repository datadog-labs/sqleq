// Unless explicitly stated otherwise all files in this repository are licensed under the
// Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/).
// Copyright 2026-Present Datadog, Inc.

//! The Postgres type of an expression, as far as deciding whether DuckDB computes it as Postgres
//! does, and what follows from it for a pair.
//!
//! DuckDB and Postgres agree on most of what they compute over this crate's small domain, but not on
//! everything, and where they disagree by *type* no setting closes the gap:
//!
//! * **A `numeric` division.** DuckDB divides a `DECIMAL` into a `DOUBLE`, where Postgres divides to
//!   a finite decimal scale (`2 / 3 * 3` is `2.00000000000000000001` there, and `2` in a DOUBLE). The
//!   scale Postgres picks depends on the operands' values and can need more digits than DuckDB's
//!   38, so there is no faithful rendering, and a pair that divides a `numeric` gets no verdict.
//! * **`power`, `pow` and `exp` of a `numeric`.** Postgres computes them in `numeric`; DuckDB in a
//!   DOUBLE. Over `double precision` (an integer argument is one) the two agree but for the inputs
//!   Postgres refuses, which `crate::rewrite::postgres_operators` makes DuckDB refuse too. The `^`
//!   operator is the same function, and an operator cannot be renamed to a macro that refuses, so a
//!   pair using it gets no verdict either.
//! * **A `double precision` turned into text.** DuckDB prints `2.0` where Postgres prints `2`, and
//!   `1000000000000000.0` where Postgres prints `1e+15`: a cast to a string type, `||` and `concat`
//!   over a float withhold the pair.
//! * **A `jsonb` literal.** DuckDB compares JSON as text; `jsonb` keeps neither key order,
//!   duplicate keys nor whitespace. A string literal that meets a `jsonb` value is rewritten into the
//!   one spelling the generated documents use ([`jsonb_literals`]), and one that might meet one in a
//!   place this module cannot read withholds the pair.
//!
//! Typing is deliberately conservative. A column is typed by name over every table the DDL declares
//! (a name declared with two types, or also used as an alias of another type, is unknown), and a
//! construct this module does not know is [`Ty::Unknown`] -- which every check above treats as the
//! type it is guarding against. So a wrong answer here can withhold a pair that needed no
//! withholding, and never the reverse.

use std::collections::{BTreeMap, HashMap};
use std::ops::ControlFlow;

use sqlparser::ast::{
    BinaryOperator, CastKind, DataType, Expr, Function, FunctionArg, FunctionArgExpr,
    FunctionArguments, Insert, ObjectName, ObjectNamePart, Query, Select, SelectItem, SetExpr,
    Statement, TableFactor, TableObject, UnaryOperator, Value, Visit, Visitor,
};
use sqlparser::dialect::PostgreSqlDialect;
use sqlparser::parser::Parser;
use sqlparser::tokenizer::{Span, Token};

use crate::lex::significant;
use crate::rewrite::byte_of;
use crate::schema::{resolve, Column, Schema, VType};

/// What Postgres types an expression as, in the classes these checks tell apart.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Ty {
    /// `smallint`, `integer`, `bigint`.
    Int,
    /// `real`, `double precision`.
    Float,
    /// `numeric`, `decimal`.
    Numeric,
    /// `text`, `varchar`, `char(n)`.
    Text,
    /// `json`, `jsonb`.
    Json,
    Interval,
    /// Anything else this module can name: `boolean`, a date or time, `uuid`, an array, ...
    Other,
    /// A string literal, `NULL` or a `$N`: Postgres types it from what it meets.
    Untyped,
    /// Not known here: may be any of the above.
    Unknown,
}

fn col_ty(c: &Column) -> Ty {
    if c.array {
        return Ty::Other;
    }
    match c.vt {
        VType::Integer => Ty::Int,
        VType::Double => Ty::Float,
        VType::Decimal(..) => Ty::Numeric,
        VType::Varchar => Ty::Text,
        VType::Json => Ty::Json,
        VType::Interval => Ty::Interval,
        VType::Boolean | VType::Date | VType::Timestamp | VType::TimestampTz | VType::Uuid => {
            Ty::Other
        }
    }
}

/// The class of a cast target.
fn type_ty(t: &DataType) -> Ty {
    use DataType as D;
    match t {
        D::Int(_)
        | D::Integer(_)
        | D::Int2(_)
        | D::Int4(_)
        | D::Int8(_)
        | D::SmallInt(_)
        | D::BigInt(_)
        | D::TinyInt(_)
        | D::MediumInt(_)
        | D::Int16
        | D::Int32
        | D::Int64
        | D::HugeInt => Ty::Int,
        D::Float(_)
        | D::Float4
        | D::Float8
        | D::Float32
        | D::Float64
        | D::Real
        | D::Double(_)
        | D::DoublePrecision => Ty::Float,
        D::Numeric(_) | D::Decimal(_) | D::Dec(_) | D::BigNumeric(_) | D::BigDecimal(_) => {
            Ty::Numeric
        }
        D::Text
        | D::Varchar(_)
        | D::Nvarchar(_)
        | D::Char(_)
        | D::Character(_)
        | D::CharacterVarying(_)
        | D::CharVarying(_)
        | D::String(_) => Ty::Text,
        D::JSON | D::JSONB => Ty::Json,
        D::Interval { .. } => Ty::Interval,
        D::Bool
        | D::Boolean
        | D::Date
        | D::Time(..)
        | D::Timestamp(..)
        | D::Datetime(_)
        | D::Uuid
        | D::Bytea
        | D::Array(_) => Ty::Other,
        D::Custom(name, _) => match last(name).as_deref() {
            Some("bpchar" | "name" | "citext") => Ty::Text,
            Some("timestamptz" | "timetz" | "inet" | "cidr" | "money") => Ty::Other,
            _ => Ty::Unknown,
        },
        _ => Ty::Unknown,
    }
}

fn last(name: &ObjectName) -> Option<String> {
    name.0.last().and_then(|p| match p {
        ObjectNamePart::Identifier(id) => Some(id.value.to_lowercase()),
        _ => None,
    })
}

/// Whether a type is a string type, so that a cast to it prints its operand.
fn is_string_type(t: &DataType) -> bool {
    type_ty(t) == Ty::Text
}

/// Names to types: every column the DDL declares, and every alias a statement defines.
pub struct Types {
    names: HashMap<String, Ty>,
}

/// One name's type, merged with what it already had: two different types make it unknown.
fn merge(names: &mut HashMap<String, Ty>, name: String, ty: Ty) {
    names
        .entry(name)
        .and_modify(|t| {
            if *t != ty {
                *t = Ty::Unknown
            }
        })
        .or_insert(ty);
}

impl Types {
    /// The types of `schema`'s columns, and of the aliases `stmts` define in terms of them.
    pub fn new(schema: &Schema, stmts: &[Statement]) -> Types {
        let mut names: HashMap<String, Ty> = HashMap::new();
        for t in schema.values() {
            for c in &t.cols {
                merge(&mut names, c.name.clone(), col_ty(c));
            }
        }
        let columns = Types {
            names: names.clone(),
        };
        let mut aliases = Aliases::default();
        for st in stmts {
            let _ = st.visit(&mut aliases);
        }
        for (name, e) in aliases.typed {
            let ty = match columns.of(&e) {
                Ty::Untyped => Ty::Unknown,
                t => t,
            };
            merge(&mut names, name, ty);
        }
        for (name, branches) in aliases.unions {
            let ty = branches
                .iter()
                .map(|e| columns.of(e))
                .reduce(unify)
                .unwrap_or(Ty::Unknown);
            merge(&mut names, name, ty);
        }
        for name in aliases.untyped {
            merge(&mut names, name, Ty::Unknown);
        }
        Types { names }
    }

    /// The class of `e`.
    pub fn of(&self, e: &Expr) -> Ty {
        match e {
            Expr::Identifier(id) => self.name(&id.value),
            Expr::CompoundIdentifier(parts) => match parts.last() {
                Some(id) => self.name(&id.value),
                None => Ty::Unknown,
            },
            Expr::Value(v) => value_ty(&v.value),
            Expr::TypedString(ts) => type_ty(&ts.data_type),
            Expr::Cast { data_type, .. } => type_ty(data_type),
            Expr::Nested(e) | Expr::Collate { expr: e, .. } => self.of(e),
            Expr::UnaryOp {
                op: UnaryOperator::Minus | UnaryOperator::Plus,
                expr,
            } => self.of(expr),
            Expr::UnaryOp { .. } => Ty::Unknown,
            Expr::BinaryOp { left, op, right } => self.binary(left, op, right),
            Expr::IsNull(_)
            | Expr::IsNotNull(_)
            | Expr::IsTrue(_)
            | Expr::IsNotTrue(_)
            | Expr::IsFalse(_)
            | Expr::IsNotFalse(_)
            | Expr::IsUnknown(_)
            | Expr::IsNotUnknown(_)
            | Expr::IsDistinctFrom(..)
            | Expr::IsNotDistinctFrom(..)
            | Expr::InList { .. }
            | Expr::InSubquery { .. }
            | Expr::InUnnest { .. }
            | Expr::Between { .. }
            | Expr::Like { .. }
            | Expr::ILike { .. }
            | Expr::SimilarTo { .. }
            | Expr::AnyOp { .. }
            | Expr::AllOp { .. }
            | Expr::Exists { .. } => Ty::Other,
            Expr::Case {
                conditions,
                else_result,
                ..
            } => conditions
                .iter()
                .map(|c| &c.result)
                .chain(else_result.as_deref())
                .map(|r| self.of(r))
                .reduce(unify)
                .unwrap_or(Ty::Unknown),
            Expr::Subquery(q) => self.query(q),
            Expr::Extract { .. } => Ty::Numeric,
            Expr::Ceil { expr, .. } | Expr::Floor { expr, .. } => rounded(self.of(expr)),
            Expr::Position { .. } => Ty::Int,
            Expr::Substring { .. } | Expr::Trim { .. } | Expr::Overlay { .. } => Ty::Text,
            Expr::Interval(_) => Ty::Interval,
            Expr::AtTimeZone { .. } | Expr::Array(_) | Expr::Tuple(_) => Ty::Other,
            Expr::Function(f) => self.call(f),
            _ => Ty::Unknown,
        }
    }

    fn name(&self, n: &str) -> Ty {
        self.names
            .get(&n.to_lowercase())
            .copied()
            .unwrap_or(Ty::Unknown)
    }

    /// A scalar subquery's type: its one projected expression's.
    fn query(&self, q: &Query) -> Ty {
        match &*q.body {
            SetExpr::Select(sel) => match sel.projection.as_slice() {
                [SelectItem::UnnamedExpr(e)] | [SelectItem::ExprWithAlias { expr: e, .. }] => {
                    self.of(e)
                }
                _ => Ty::Unknown,
            },
            SetExpr::Query(q) => self.query(q),
            _ => Ty::Unknown,
        }
    }

    fn binary(&self, left: &Expr, op: &BinaryOperator, right: &Expr) -> Ty {
        use BinaryOperator as B;
        match op {
            B::Plus | B::Minus | B::Multiply | B::Divide | B::Modulo => {
                arith(self.of(left), self.of(right))
            }
            B::StringConcat => match (self.of(left), self.of(right)) {
                (Ty::Json, Ty::Json | Ty::Untyped) | (Ty::Untyped, Ty::Json) => Ty::Json,
                (Ty::Text, _) | (_, Ty::Text) => Ty::Text,
                _ => Ty::Unknown,
            },
            B::Arrow | B::HashArrow => Ty::Json,
            B::LongArrow | B::HashLongArrow => Ty::Text,
            B::Gt
            | B::Lt
            | B::GtEq
            | B::LtEq
            | B::Eq
            | B::NotEq
            | B::Spaceship
            | B::And
            | B::Or
            | B::Xor
            | B::PGRegexMatch
            | B::PGRegexIMatch
            | B::PGRegexNotMatch
            | B::PGRegexNotIMatch
            | B::PGLikeMatch
            | B::PGILikeMatch
            | B::PGNotLikeMatch
            | B::PGNotILikeMatch
            | B::AtArrow
            | B::ArrowAt
            | B::Question
            | B::QuestionAnd
            | B::QuestionPipe
            | B::PGOverlap => Ty::Other,
            B::BitwiseAnd
            | B::BitwiseOr
            | B::BitwiseXor
            | B::PGBitwiseXor
            | B::PGBitwiseShiftLeft
            | B::PGBitwiseShiftRight => match self.of(left) {
                Ty::Int => Ty::Int,
                _ => Ty::Unknown,
            },
            B::PGExp => match (self.of(left), self.of(right)) {
                (Ty::Numeric | Ty::Unknown, _) | (_, Ty::Numeric | Ty::Unknown) => Ty::Unknown,
                _ => Ty::Float,
            },
            _ => Ty::Unknown,
        }
    }

    /// The argument expressions of a plain call, `None` for anything else (`*`, named arguments).
    fn args<'f>(&self, f: &'f Function) -> Option<Vec<&'f Expr>> {
        let FunctionArguments::List(list) = &f.args else {
            return match &f.args {
                FunctionArguments::None => Some(Vec::new()),
                _ => None,
            };
        };
        list.args
            .iter()
            .map(|a| match a {
                FunctionArg::Unnamed(FunctionArgExpr::Expr(e)) => Some(e),
                _ => None,
            })
            .collect()
    }

    fn call(&self, f: &Function) -> Ty {
        let Some(name) = last(&f.name) else {
            return Ty::Unknown;
        };
        let args = self.args(f);
        let tys: Vec<Ty> = args
            .as_deref()
            .unwrap_or(&[])
            .iter()
            .map(|e| self.of(e))
            .collect();
        let first = tys.first().copied().unwrap_or(Ty::Unknown);
        let all = || tys.iter().copied().reduce(unify).unwrap_or(Ty::Unknown);
        match name.as_str() {
            "count" | "row_number" | "rank" | "dense_rank" | "ntile" | "length" | "char_length"
            | "character_length" | "octet_length" | "bit_length" | "strpos" | "ascii"
            | "array_length" | "cardinality" | "array_position" | "width_bucket" => Ty::Int,
            "percent_rank" | "cume_dist" | "random" | "pi" | "date_part" | "degrees"
            | "radians" | "sin" | "cos" | "tan" | "asin" | "acos" | "atan" | "atan2" | "cbrt" => {
                Ty::Float
            }
            "sum" => match first {
                Ty::Int | Ty::Float | Ty::Numeric | Ty::Interval => first,
                _ => Ty::Unknown,
            },
            "avg" | "stddev" | "stddev_pop" | "stddev_samp" | "variance" | "var_pop"
            | "var_samp" => match first {
                Ty::Int | Ty::Numeric => Ty::Numeric,
                Ty::Float | Ty::Interval => first,
                _ => Ty::Unknown,
            },
            "min" | "max" | "abs" | "coalesce" | "greatest" | "least" | "any_value" | "lag"
            | "lead" | "first_value" | "last_value" | "nth_value" | "mod" => match name.as_str() {
                "lag" | "lead" | "first_value" | "last_value" | "nth_value" => first,
                _ => all(),
            },
            "nullif" => first,
            "ceil" | "ceiling" | "floor" | "sign" | "round" | "trunc" if tys.len() == 1 => {
                rounded(first)
            }
            "round" | "trunc" | "div" | "to_number" | "log" if tys.len() == 2 => Ty::Numeric,
            "power" | "pow" | "exp" | "ln" | "log" | "log10" | "sqrt" => {
                if tys.iter().any(|t| matches!(t, Ty::Numeric | Ty::Unknown)) || args.is_none() {
                    // `power` over a `numeric` is `numeric`; an unknown argument may be one.
                    if tys.contains(&Ty::Numeric) {
                        Ty::Numeric
                    } else {
                        Ty::Unknown
                    }
                } else {
                    Ty::Float
                }
            }
            "lower" | "upper" | "initcap" | "concat" | "concat_ws" | "substr" | "left"
            | "right" | "lpad" | "rpad" | "ltrim" | "rtrim" | "btrim" | "replace"
            | "translate" | "reverse" | "repeat" | "split_part" | "md5" | "to_char" | "format"
            | "string_agg" | "quote_ident" | "quote_literal" | "regexp_replace" | "chr"
            | "json_extract_path_text" | "jsonb_extract_path_text" | "json_typeof"
            | "jsonb_typeof" | "array_to_string" => Ty::Text,
            "to_json" | "to_jsonb" | "json_build_object" | "jsonb_build_object" | "json_agg"
            | "jsonb_agg" | "json_object_agg" | "jsonb_object_agg" | "jsonb_set"
            | "json_build_array" | "jsonb_build_array" | "row_to_json" | "array_to_json"
            | "json_extract_path" | "jsonb_extract_path" | "jsonb_strip_nulls"
            | "jsonb_insert" | "json_array" | "json_object" | "json_arrayagg"
            | "json_objectagg" => Ty::Json,
            "age" | "justify_days" | "justify_hours" | "justify_interval" | "make_interval" => {
                Ty::Interval
            }
            "now" | "date_trunc" | "to_timestamp" | "to_date" | "make_date" | "bool_and"
            | "bool_or" | "every" | "array_agg" => Ty::Other,
            _ => Ty::Unknown,
        }
    }
}

/// `ceil`, `floor`, `round`, `trunc` and `sign` of one argument: Postgres has them for `numeric` and
/// `double precision`, and an integer argument resolves to the latter.
fn rounded(t: Ty) -> Ty {
    match t {
        Ty::Int | Ty::Float | Ty::Untyped => Ty::Float,
        Ty::Numeric => Ty::Numeric,
        _ => Ty::Unknown,
    }
}

fn value_ty(v: &Value) -> Ty {
    match v {
        Value::Number(n, _) => {
            if n.contains(['.', 'e', 'E']) || n.parse::<i64>().is_err() {
                Ty::Numeric // `1.5`, `1e3`, and an integer past `bigint` are `numeric` literals
            } else {
                Ty::Int
            }
        }
        Value::SingleQuotedString(_)
        | Value::DollarQuotedString(_)
        | Value::EscapedStringLiteral(_)
        | Value::UnicodeStringLiteral(_)
        | Value::NationalStringLiteral(_)
        | Value::Null
        | Value::Placeholder(_) => Ty::Untyped,
        Value::Boolean(_) => Ty::Other,
        _ => Ty::Unknown,
    }
}

/// The type of an arithmetic operator's result over operands of these types. An untyped operand
/// takes the other's type, as Postgres resolves it; `numeric` with `double precision` is `double
/// precision`, since `numeric` casts to it implicitly.
fn arith(l: Ty, r: Ty) -> Ty {
    match (l, r) {
        (Ty::Untyped, Ty::Untyped) => Ty::Unknown,
        (Ty::Untyped, t) | (t, Ty::Untyped) => t,
        (Ty::Unknown, _) | (_, Ty::Unknown) => Ty::Unknown,
        (Ty::Float, Ty::Int | Ty::Float | Ty::Numeric) | (Ty::Int | Ty::Numeric, Ty::Float) => {
            Ty::Float
        }
        (Ty::Numeric, Ty::Int | Ty::Numeric) | (Ty::Int, Ty::Numeric) => Ty::Numeric,
        (Ty::Int, Ty::Int) => Ty::Int,
        (Ty::Interval, Ty::Int | Ty::Float | Ty::Numeric | Ty::Interval)
        | (Ty::Int | Ty::Float | Ty::Numeric, Ty::Interval) => Ty::Interval,
        _ => Ty::Unknown,
    }
}

/// The common type of several branches (`CASE`, `COALESCE`, `UNION`): an untyped one takes the
/// others', and two numeric classes the wider.
fn unify(a: Ty, b: Ty) -> Ty {
    match (a, b) {
        (Ty::Untyped, t) | (t, Ty::Untyped) => t,
        (a, b) if a == b => a,
        (Ty::Int | Ty::Numeric | Ty::Float, Ty::Int | Ty::Numeric | Ty::Float) => {
            if a == Ty::Float || b == Ty::Float {
                Ty::Float
            } else {
                Ty::Numeric
            }
        }
        _ => Ty::Unknown,
    }
}

/// The aliases a statement defines: a select-list alias with its expression, the output names of a
/// set operation with each branch's expression, and the column list of a table alias or a CTE,
/// whose types are not followed here.
#[derive(Default)]
struct Aliases {
    typed: Vec<(String, Expr)>,
    unions: Vec<(String, Vec<Expr>)>,
    untyped: Vec<String>,
}

impl Aliases {
    fn alias_columns(&mut self, alias: Option<&sqlparser::ast::TableAlias>) {
        if let Some(a) = alias {
            for c in &a.columns {
                self.untyped.push(c.name.value.to_lowercase());
            }
        }
    }
}

/// The selects of a set operation, left to right.
fn branches(body: &SetExpr, out: &mut Vec<Option<Select>>) {
    match body {
        SetExpr::Select(sel) => out.push(Some((**sel).clone())),
        SetExpr::Query(q) => branches(&q.body, out),
        SetExpr::SetOperation { left, right, .. } => {
            branches(left, out);
            branches(right, out);
        }
        _ => out.push(None),
    }
}

/// The name a select-list item gives its column, if it gives it one this module can see.
fn output_name(item: &SelectItem) -> Option<String> {
    match item {
        SelectItem::ExprWithAlias { alias, .. } => Some(alias.value.to_lowercase()),
        SelectItem::UnnamedExpr(Expr::Identifier(id)) => Some(id.value.to_lowercase()),
        SelectItem::UnnamedExpr(Expr::CompoundIdentifier(p)) => {
            p.last().map(|id| id.value.to_lowercase())
        }
        _ => None,
    }
}

impl Visitor for Aliases {
    type Break = ();

    fn pre_visit_query(&mut self, q: &Query) -> ControlFlow<()> {
        if let Some(with) = &q.with {
            for cte in &with.cte_tables {
                self.alias_columns(Some(&cte.alias));
            }
        }
        if let SetExpr::SetOperation { .. } = &*q.body {
            let mut sels = Vec::new();
            branches(&q.body, &mut sels);
            let Some(Some(head)) = sels.first() else {
                return ControlFlow::Continue(());
            };
            for (i, item) in head.projection.iter().enumerate() {
                let Some(name) = output_name(item) else {
                    continue;
                };
                let exprs: Option<Vec<Expr>> = sels
                    .iter()
                    .map(|s| match s.as_ref()?.projection.get(i)? {
                        SelectItem::UnnamedExpr(e) | SelectItem::ExprWithAlias { expr: e, .. } => {
                            Some(e.clone())
                        }
                        _ => None,
                    })
                    .collect();
                match exprs {
                    Some(exprs) => self.unions.push((name, exprs)),
                    None => self.untyped.push(name),
                }
            }
        }
        ControlFlow::Continue(())
    }

    fn pre_visit_select(&mut self, sel: &Select) -> ControlFlow<()> {
        for item in &sel.projection {
            if let SelectItem::ExprWithAlias { expr, alias } = item {
                self.typed.push((alias.value.to_lowercase(), expr.clone()));
            }
        }
        ControlFlow::Continue(())
    }

    fn pre_visit_table_factor(&mut self, f: &TableFactor) -> ControlFlow<()> {
        match f {
            TableFactor::Table { alias, .. }
            | TableFactor::Derived { alias, .. }
            | TableFactor::TableFunction { alias, .. }
            | TableFactor::Function { alias, .. }
            | TableFactor::UNNEST { alias, .. } => self.alias_columns(alias.as_ref()),
            _ => {}
        }
        ControlFlow::Continue(())
    }
}

/// Why DuckDB cannot be made to compute this statement as Postgres does, if it cannot: a division
/// of a `numeric`, `power`/`pow`/`exp` of one, the `^` operator, or a `double precision` turned into
/// text (see the module docs). A statement that does not parse is withheld if its tokens show any
/// of these.
pub fn unmodelled(sql: &str, schema: &Schema) -> Option<String> {
    let Ok(stmts) = Parser::parse_sql(&PostgreSqlDialect {}, sql) else {
        let toks = significant(sql)?;
        let risky = toks.iter().any(|t| match &t.token {
            Token::Div | Token::Caret | Token::StringConcat | Token::DoubleColon => true,
            Token::Word(w) => {
                w.quote_style.is_none()
                    && ["cast", "power", "pow", "exp", "concat", "concat_ws"]
                        .contains(&w.value.to_lowercase().as_str())
            }
            _ => false,
        });
        return risky.then(|| {
            "a division, a power or a cast to text in a statement that does not parse".to_string()
        });
    };
    let types = Types::new(schema, &stmts);
    let mut check = Unmodelled {
        types: &types,
        why: None,
    };
    for st in &stmts {
        let _ = st.visit(&mut check);
    }
    check.why
}

struct Unmodelled<'a> {
    types: &'a Types,
    why: Option<String>,
}

/// `Unknown` counts as the type being guarded against.
fn maybe(t: Ty, guarded: Ty) -> bool {
    t == guarded || t == Ty::Unknown
}

impl Visitor for Unmodelled<'_> {
    type Break = ();

    fn pre_visit_expr(&mut self, e: &Expr) -> ControlFlow<()> {
        let types = self.types;
        let why: Option<&str> = match e {
            Expr::BinaryOp {
                left,
                op: BinaryOperator::Divide,
                right,
            } => {
                let (l, r) = (types.of(left), types.of(right));
                // A `double precision` on either side makes it a float division, which DuckDB does
                // as Postgres does; two untyped operands are not something to guess about.
                let float = l == Ty::Float || r == Ty::Float;
                let untyped = l == Ty::Untyped && r == Ty::Untyped;
                (!float && (maybe(l, Ty::Numeric) || maybe(r, Ty::Numeric) || untyped)).then_some(
                    "a division of a numeric value (DuckDB divides a DECIMAL into a DOUBLE)",
                )
            }
            Expr::BinaryOp {
                op: BinaryOperator::PGExp,
                ..
            } => Some("the ^ operator (Postgres raises where DuckDB answers inf or NaN)"),
            Expr::BinaryOp {
                left,
                op: BinaryOperator::StringConcat,
                right,
            } => {
                let (l, r) = (types.of(left), types.of(right));
                let printed = |t: Ty, other: Ty| {
                    maybe(t, Ty::Float) && !matches!(other, Ty::Json | Ty::Other)
                };
                (printed(l, r) || printed(r, l))
                    .then_some("a double precision value turned into text by ||")
            }
            Expr::Cast {
                kind: CastKind::Cast | CastKind::DoubleColon | CastKind::TryCast | CastKind::SafeCast,
                expr,
                data_type,
                ..
            } if is_string_type(data_type) && maybe(types.of(expr), Ty::Float) => {
                Some("a double precision value cast to text (DuckDB prints 2.0 where Postgres prints 2)")
            }
            Expr::Function(f) => match last(&f.name).as_deref() {
                Some("power" | "pow" | "exp") => {
                    let numeric = match types.args(f) {
                        Some(args) => args.iter().any(|a| maybe(types.of(a), Ty::Numeric)),
                        None => true,
                    };
                    numeric.then_some(
                        "power or exp of a numeric value (DuckDB computes it in a DOUBLE)",
                    )
                }
                Some("concat" | "concat_ws") => {
                    let float = match types.args(f) {
                        Some(args) => args.iter().any(|a| maybe(types.of(a), Ty::Float)),
                        None => true,
                    };
                    float.then_some("a double precision value turned into text by concat")
                }
                _ => None,
            },
            _ => None,
        };
        if let Some(why) = why {
            self.why.get_or_insert_with(|| why.to_string());
            return ControlFlow::Break(());
        }
        ControlFlow::Continue(())
    }
}

/// The one spelling of a `jsonb` value: keys sorted as `jsonb` sorts them -- shorter first, then
/// bytewise -- the last of duplicate keys kept, and no whitespace. It is the spelling of every
/// document `crate::gen::JSONS` holds and of every object the `jsonb_build_object` shim writes, so
/// two values in it are equal as text exactly when they are equal as `jsonb`.
///
/// `None` for a document whose `jsonb` value this cannot spell exactly: a number that is not an
/// integer (`jsonb` compares `1.0` equal to `1`, and keeps a precision a binary float does not), or a
/// string holding `\u0000`, which `jsonb` refuses.
pub fn canonical_jsonb(v: &serde_json::Value) -> Option<String> {
    use serde_json::Value as J;
    Some(match v {
        J::Null => "null".to_string(),
        J::Bool(b) => b.to_string(),
        J::Number(n) if n.is_i64() || n.is_u64() => n.to_string(),
        J::Number(_) => return None,
        J::String(s) if s.contains('\0') => return None,
        J::String(_) => v.to_string(),
        J::Array(items) => {
            let parts: Option<Vec<String>> = items.iter().map(canonical_jsonb).collect();
            format!("[{}]", parts?.join(","))
        }
        J::Object(map) => {
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort_by(|a, b| a.len().cmp(&b.len()).then_with(|| a.cmp(b)));
            let mut parts = Vec::with_capacity(keys.len());
            for k in keys {
                if k.contains('\0') {
                    return None;
                }
                let key = serde_json::Value::String(k.clone()).to_string();
                parts.push(format!("{key}:{}", canonical_jsonb(&map[k])?));
            }
            format!("{{{}}}", parts.join(","))
        }
    })
}

/// What happens to one string literal.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Fate {
    /// It meets a `jsonb` value: respelled canonically.
    Jsonb,
    /// It is read as something else (text, `json`, a pattern): left alone.
    Other,
}

/// Rewrite every string literal that meets a `jsonb` value into the canonical spelling
/// ([`canonical_jsonb`]): one compared with a json-typed expression (`=`, `<>`, `IS [NOT] DISTINCT
/// FROM`, an `IN` list), cast to `jsonb`, or written into a json column by `INSERT ... VALUES` or
/// `UPDATE ... SET`. `Err` withholds the pair, with why:
///
/// * such a literal whose value has no exact canonical spelling;
/// * an ordering comparison (`<`, `>`, ...) of a json-typed value, since DuckDB orders JSON as text;
/// * a literal spelling a JSON object or array that is not canonical, in a place this does not read
///   (a function argument, a `CASE` branch, a peer of unknown type): it may meet a `jsonb` there.
///
/// Every edit is a whole literal token replaced, and the result must parse to the input's tree with
/// those literals replaced; anything else withholds the pair. A statement that does not parse is
/// returned as it is.
pub fn jsonb_literals(sql: &str, schema: &Schema) -> Result<String, String> {
    let Ok(stmts) = Parser::parse_sql(&PostgreSqlDialect {}, sql) else {
        return Ok(sql.to_string());
    };
    let types = Types::new(schema, &stmts);
    let mut walk = Literals {
        types: &types,
        schema,
        fates: HashMap::new(),
        literals: Vec::new(),
        refused: None,
    };
    for st in &stmts {
        let _ = st.visit(&mut walk);
    }
    if let Some(why) = walk.refused {
        return Err(why);
    }
    // (start, end) byte range -> the canonical literal that replaces it.
    let mut edits: BTreeMap<usize, (usize, String)> = BTreeMap::new();
    let mut replaced: HashMap<SpanKey, String> = HashMap::new();
    for (span, text) in &walk.literals {
        let fate = walk.fates.get(&key(span)).copied();
        let parsed: Option<serde_json::Value> = serde_json::from_str(text).ok();
        let canonical = parsed.as_ref().map(canonical_jsonb);
        match (fate, &parsed, canonical) {
            // Not JSON at all: casting it to `jsonb` raises in both engines.
            (Some(Fate::Jsonb), None, _) => {}
            (Some(Fate::Jsonb), Some(_), Some(None)) => {
                return Err(format!(
                    "a jsonb literal with no exact canonical spelling: {text}"
                ))
            }
            (Some(Fate::Jsonb), Some(_), Some(Some(c))) => {
                if &c != text {
                    let (Some(s), Some(e)) = (byte_of(sql, span.start), byte_of(sql, span.end))
                    else {
                        return Err("a jsonb literal whose position could not be found".into());
                    };
                    edits.insert(s, (e, format!("'{}'", c.replace('\'', "''"))));
                    replaced.insert(key(span), c);
                }
            }
            (Some(Fate::Jsonb), Some(_), None) => unreachable!("parsed implies a canonical"),
            (Some(Fate::Other), ..) => {}
            (None, Some(v), canonical) if v.is_object() || v.is_array() => {
                if canonical.flatten().as_deref() != Some(text.as_str()) {
                    return Err(format!(
                        "a JSON literal not in jsonb's spelling, where it may meet a jsonb: {text}"
                    ));
                }
            }
            (None, ..) => {}
        }
    }
    if edits.is_empty() {
        return Ok(sql.to_string());
    }
    let mut out = String::with_capacity(sql.len());
    let mut cur = 0;
    for (s, (e, text)) in &edits {
        if *s < cur || !sql.is_char_boundary(*s) || !sql.is_char_boundary(*e) {
            return Err("a jsonb literal whose position could not be found".into());
        }
        out.push_str(&sql[cur..*s]);
        out.push_str(text);
        cur = *e;
    }
    out.push_str(&sql[cur..]);
    let expected: Vec<Statement> = {
        let mut e = stmts.clone();
        for st in &mut e {
            let _ = sqlparser::ast::VisitMut::visit(st, &mut Respell(&replaced));
        }
        e
    };
    match Parser::parse_sql(&PostgreSqlDialect {}, &out) {
        Ok(after) if after == expected => Ok(out),
        _ => Err("a jsonb literal whose position could not be found".into()),
    }
}

/// The literals [`jsonb_literals`] respells, by their place in the input, as the parse of its output
/// has them.
struct Respell<'a>(&'a HashMap<SpanKey, String>);

impl sqlparser::ast::VisitorMut for Respell<'_> {
    type Break = ();

    fn post_visit_value(&mut self, v: &mut sqlparser::ast::ValueWithSpan) -> ControlFlow<()> {
        if let Some(c) = self.0.get(&key(&v.span)) {
            if let Value::SingleQuotedString(s) = &mut v.value {
                *s = c.clone();
            }
        }
        ControlFlow::Continue(())
    }
}

type SpanKey = (u64, u64, u64, u64);

fn key(s: &Span) -> SpanKey {
    (s.start.line, s.start.column, s.end.line, s.end.column)
}

struct Literals<'a> {
    types: &'a Types,
    schema: &'a Schema,
    fates: HashMap<SpanKey, Fate>,
    /// Every single-quoted literal, with its span and text.
    literals: Vec<(Span, String)>,
    refused: Option<String>,
}

/// The span of `e` if it is a string literal, through parentheses; `Err` for a string literal in a
/// spelling other than `'...'`, which is not respelled.
fn literal(e: &Expr) -> Option<Result<Span, ()>> {
    match e {
        Expr::Nested(inner) => literal(inner),
        Expr::Value(v) => match &v.value {
            Value::SingleQuotedString(_) => Some(Ok(v.span)),
            Value::DollarQuotedString(_)
            | Value::EscapedStringLiteral(_)
            | Value::UnicodeStringLiteral(_)
            | Value::NationalStringLiteral(_) => Some(Err(())),
            _ => None,
        },
        _ => None,
    }
}

impl Literals<'_> {
    /// Settle a literal's fate by the type of what it meets.
    fn meets(&mut self, lit: &Expr, peer: Ty) {
        let fate = match peer {
            Ty::Json => Fate::Jsonb,
            Ty::Unknown => return,
            _ => Fate::Other,
        };
        self.settle(lit, fate);
    }

    fn settle(&mut self, lit: &Expr, fate: Fate) {
        match literal(lit) {
            Some(Ok(span)) => {
                self.fates.insert(key(&span), fate);
            }
            Some(Err(())) if fate == Fate::Jsonb => {
                self.refused
                    .get_or_insert("a jsonb literal in a spelling other than '...'".into());
            }
            _ => {}
        }
    }

    /// The json-typed target columns of an `INSERT`, in `VALUES` order.
    fn insert(&mut self, ins: &Insert) {
        let TableObject::TableName(name) = &ins.table else {
            return;
        };
        let parts: Vec<String> = name
            .0
            .iter()
            .filter_map(|p| match p {
                ObjectNamePart::Identifier(id) => Some(id.value.to_lowercase()),
                _ => None,
            })
            .collect();
        let Some(table) = resolve(self.schema, &parts).map(|k| &self.schema[k]) else {
            return;
        };
        let targets: Vec<Option<Ty>> = if ins.columns.is_empty() {
            table.cols.iter().map(|c| Some(col_ty(c))).collect()
        } else {
            ins.columns
                .iter()
                .map(|n| {
                    let n = last(n)?;
                    table.cols.iter().find(|c| c.name == n).map(col_ty)
                })
                .collect()
        };
        let Some(source) = &ins.source else { return };
        if let SetExpr::Values(values) = &*source.body {
            for row in &values.rows {
                for (e, t) in row.iter().zip(&targets) {
                    if let Some(t) = t {
                        self.meets(e, *t);
                    }
                }
            }
        }
    }
}

impl Visitor for Literals<'_> {
    type Break = ();

    fn pre_visit_statement(&mut self, st: &Statement) -> ControlFlow<()> {
        match st {
            Statement::Insert(ins) => self.insert(ins),
            Statement::Update(up) => {
                for a in &up.assignments {
                    if let sqlparser::ast::AssignmentTarget::ColumnName(n) = &a.target {
                        if let Some(n) = last(n) {
                            let t = self.types.name(&n);
                            self.meets(&a.value, t);
                        }
                    }
                }
            }
            _ => {}
        }
        ControlFlow::Continue(())
    }

    fn pre_visit_expr(&mut self, e: &Expr) -> ControlFlow<()> {
        use BinaryOperator as B;
        let types = self.types;
        match e {
            Expr::Value(v) => {
                if let Value::SingleQuotedString(s) = &v.value {
                    self.literals.push((v.span, s.clone()));
                }
            }
            Expr::BinaryOp { left, op, right } => match op {
                B::Eq | B::NotEq => {
                    self.meets(left, types.of(right));
                    self.meets(right, types.of(left));
                }
                B::Lt | B::LtEq | B::Gt | B::GtEq
                    if types.of(left) == Ty::Json || types.of(right) == Ty::Json =>
                {
                    self.refused.get_or_insert(
                        "an ordering comparison of json values (DuckDB orders JSON as text)".into(),
                    );
                }
                // A pattern, a regex, a string operand: read as text.
                B::StringConcat
                | B::PGLikeMatch
                | B::PGILikeMatch
                | B::PGNotLikeMatch
                | B::PGNotILikeMatch
                | B::PGRegexMatch
                | B::PGRegexIMatch
                | B::PGRegexNotMatch
                | B::PGRegexNotIMatch
                | B::LongArrow
                | B::Arrow
                | B::HashArrow
                | B::HashLongArrow => {
                    // The key of an accessor is text; the document it reads is the left operand.
                    if matches!(op, B::Arrow | B::LongArrow | B::HashArrow | B::HashLongArrow) {
                        self.settle(right, Fate::Other);
                    } else {
                        self.settle(left, Fate::Other);
                        self.settle(right, Fate::Other);
                    }
                }
                _ => {}
            },
            Expr::IsDistinctFrom(l, r) | Expr::IsNotDistinctFrom(l, r) => {
                self.meets(l, types.of(r));
                self.meets(r, types.of(l));
            }
            Expr::InList { expr, list, .. } => {
                let t = types.of(expr);
                for item in list {
                    self.meets(item, t);
                }
                for item in list {
                    if types.of(item) == Ty::Json {
                        self.meets(expr, Ty::Json);
                    }
                }
            }
            Expr::Like { expr, pattern, .. } | Expr::ILike { expr, pattern, .. } => {
                self.settle(expr, Fate::Other);
                self.settle(pattern, Fate::Other);
            }
            Expr::Cast {
                expr, data_type, ..
            } => match data_type {
                DataType::JSONB => self.settle(expr, Fate::Jsonb),
                // Only a `jsonb` normalizes; a `json` keeps its text, as DuckDB's JSON does.
                _ => self.settle(expr, Fate::Other),
            },
            Expr::TypedString(ts) => {
                if let Value::SingleQuotedString(s) = &ts.value.value {
                    self.literals.push((ts.value.span, s.clone()));
                    let fate = if ts.data_type == DataType::JSONB {
                        Fate::Jsonb
                    } else {
                        Fate::Other
                    };
                    self.fates.insert(key(&ts.value.span), fate);
                } else if ts.data_type == DataType::JSONB {
                    self.refused
                        .get_or_insert("a jsonb literal in a spelling other than '...'".into());
                }
            }
            _ => {}
        }
        ControlFlow::Continue(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::parse_schema;

    fn ty(sql_expr: &str, ddl: &str) -> Ty {
        let sql = format!("SELECT {sql_expr} FROM t");
        let stmts = Parser::parse_sql(&PostgreSqlDialect {}, &sql).unwrap();
        let types = Types::new(&parse_schema(ddl), &stmts);
        let Statement::Query(q) = &stmts[0] else {
            unreachable!()
        };
        let SetExpr::Select(sel) = &*q.body else {
            unreachable!()
        };
        let SelectItem::UnnamedExpr(e) = &sel.projection[0] else {
            unreachable!()
        };
        types.of(e)
    }

    const DDL: &str = "create table t (i int, b bigint, f float8, r real, n numeric(10,2), \
                       m numeric, s text, j jsonb, x interval, d date)";

    #[test]
    fn columns_literals_and_casts_are_typed_as_postgres_types_them() {
        for (e, want) in [
            ("i", Ty::Int),
            ("t.b", Ty::Int),
            ("f", Ty::Float),
            ("r", Ty::Float),
            ("n", Ty::Numeric),
            ("m", Ty::Numeric),
            ("s", Ty::Text),
            ("j", Ty::Json),
            ("x", Ty::Interval),
            ("d", Ty::Other),
            ("nope", Ty::Unknown),
            ("1", Ty::Int),
            ("1.5", Ty::Numeric),
            ("1e3", Ty::Numeric),
            ("99999999999999999999", Ty::Numeric),
            ("'a'", Ty::Untyped),
            ("$1", Ty::Untyped),
            ("i::float8", Ty::Float),
            ("CAST(i AS numeric)", Ty::Numeric),
            ("i / 2", Ty::Int),
            ("i / 2.0", Ty::Numeric),
            ("n * f", Ty::Float),
            ("i + $1", Ty::Int),
            ("avg(i)", Ty::Numeric),
            ("sum(i)", Ty::Int),
            ("count(*)", Ty::Int),
            ("round(i)", Ty::Float),
            ("round(n, 2)", Ty::Numeric),
            ("power(i, 2)", Ty::Float),
            ("power(i, 0.5)", Ty::Numeric),
            ("coalesce(i, 2.5)", Ty::Numeric),
            ("CASE WHEN i > 0 THEN f ELSE 0 END", Ty::Float),
            ("j -> 'a'", Ty::Json),
            ("j ->> 'a'", Ty::Text),
            ("s || i", Ty::Text),
            ("extract(day from d)", Ty::Numeric),
            ("(SELECT max(n) FROM t)", Ty::Numeric),
        ] {
            assert_eq!(ty(e, DDL), want, "{e}");
        }
    }

    #[test]
    fn an_alias_is_typed_by_its_expression_and_a_clash_is_unknown() {
        let schema = parse_schema(DDL);
        let sql = "SELECT q / 3 FROM (SELECT n AS q, i AS f FROM t) s";
        let stmts = Parser::parse_sql(&PostgreSqlDialect {}, sql).unwrap();
        let types = Types::new(&schema, &stmts);
        assert_eq!(types.name("q"), Ty::Numeric);
        // `f` is a float column and an alias of an integer: either may be meant.
        assert_eq!(types.name("f"), Ty::Unknown);
        // A set operation's output column takes the type of every branch, not only the first
        // branch's alias (which would say integer); the two together are not one type.
        let sql = "SELECT k / 2 FROM (SELECT i AS k FROM t UNION ALL SELECT n FROM t) u";
        let stmts = Parser::parse_sql(&PostgreSqlDialect {}, sql).unwrap();
        assert_ne!(Types::new(&schema, &stmts).name("k"), Ty::Int);
        // A column list of a table alias is not followed.
        let sql = "SELECT z FROM (SELECT i FROM t) s(z)";
        let stmts = Parser::parse_sql(&PostgreSqlDialect {}, sql).unwrap();
        assert_eq!(Types::new(&schema, &stmts).name("z"), Ty::Unknown);
    }

    #[test]
    fn the_canonical_jsonb_spelling_sorts_dedups_and_drops_whitespace() {
        let c = |s: &str| canonical_jsonb(&serde_json::from_str(s).unwrap());
        assert_eq!(c(r#"{"b":"b", "a": 1}"#).as_deref(), Some(r#"{"a":1,"b":"b"}"#));
        assert_eq!(c(r#"{"aa":1,"b":2}"#).as_deref(), Some(r#"{"b":2,"aa":1}"#));
        assert_eq!(c(r#"{"a":1,"a":2}"#).as_deref(), Some(r#"{"a":2}"#));
        assert_eq!(c(r#"[ 1, {"c":[]} ]"#).as_deref(), Some(r#"[1,{"c":[]}]"#));
        assert_eq!(c(r#"{"a":1.0}"#), None);
        assert_eq!(c(r#"{"a":"\u0000"}"#), None);
        // Every generated document is already in it.
        for doc in crate::gen::JSONS {
            assert_eq!(c(doc).as_deref(), Some(doc));
        }
    }

    #[test]
    fn what_duckdb_computes_in_another_type_is_withheld() {
        let s = parse_schema(DDL);
        for sql in [
            "SELECT n / 3 FROM t",
            "SELECT i / 2.0 FROM t",
            "SELECT i / 1e0 FROM t",
            "SELECT avg(i) / 2 FROM t",
            "SELECT nope / 2 FROM t",
            "SELECT $1 / $2 FROM t",
            "SELECT power(n, 2) FROM t",
            "SELECT exp(1.5) FROM t",
            "SELECT pow(i, nope) FROM t",
            "SELECT i ^ 2 FROM t",
            "SELECT f::text FROM t",
            "SELECT CAST(r AS varchar(5)) FROM t",
            "SELECT (f * 2)::bpchar FROM t",
            "SELECT f || 'x' FROM t",
            "SELECT 'x' || nope FROM t",
            "SELECT concat(s, f) FROM t",
            "SELECT nope::text FROM t",
            "this is not sql / 2",
        ] {
            assert!(unmodelled(sql, &s).is_some(), "{sql}");
        }
        for sql in [
            "SELECT i / 2, b / i FROM t",
            "SELECT f / n, n / f, i / 2.0::float8 FROM t",
            "SELECT x / 2 FROM t",
            "SELECT n % 3, mod(n, 3) FROM t",
            "SELECT power(i, 2), pow($1, 2), exp(f) FROM t",
            "SELECT i::text, n::text, s || 'x', s || i, concat(s, i), $1::text FROM t",
            "SELECT j::text, x::text FROM t",
            "this is not sql at all",
        ] {
            assert_eq!(unmodelled(sql, &s), None, "{sql}");
        }
    }

    #[test]
    fn a_literal_meeting_a_jsonb_value_is_respelled() {
        let s = parse_schema(DDL);
        for (sql, want) in [
            (
                r#"SELECT 1 FROM t WHERE j = '{"b": 2, "a": 1}'"#,
                r#"SELECT 1 FROM t WHERE j = '{"a":1,"b":2}'"#,
            ),
            (
                r#"SELECT 1 FROM t WHERE '{"b":2,"a":1}' <> j"#,
                r#"SELECT 1 FROM t WHERE '{"a":1,"b":2}' <> j"#,
            ),
            (
                r#"SELECT 1 FROM t WHERE j IN ('{"aa":1,"b":2}', '{}')"#,
                r#"SELECT 1 FROM t WHERE j IN ('{"b":2,"aa":1}', '{}')"#,
            ),
            (
                r#"SELECT '[1, {"b":1,"a":2}]'::jsonb, CAST('{"a":1, "a":2}' AS jsonb)"#,
                r#"SELECT '[1,{"a":2,"b":1}]'::jsonb, CAST('{"a":2}' AS jsonb)"#,
            ),
            (
                r#"SELECT 1 FROM t WHERE j IS DISTINCT FROM '{"c": "it''s"}'"#,
                r#"SELECT 1 FROM t WHERE j IS DISTINCT FROM '{"c":"it''s"}'"#,
            ),
            (
                r#"UPDATE t SET j = '{"b":1, "a":2}' WHERE i = 1"#,
                r#"UPDATE t SET j = '{"a":2,"b":1}' WHERE i = 1"#,
            ),
            (
                r#"INSERT INTO t (j, i) VALUES ('{"b":1, "a":2}', 1)"#,
                r#"INSERT INTO t (j, i) VALUES ('{"a":2,"b":1}', 1)"#,
            ),
        ] {
            assert_eq!(jsonb_literals(sql, &s).as_deref(), Ok(want), "{sql}");
        }
        // Read as something other than a jsonb: left as written.
        for sql in [
            r#"SELECT '{"b":1, "a":2}'::json FROM t"#,
            r#"SELECT 1 FROM t WHERE s = '{"b":1, "a":2}'"#,
            r#"SELECT 1 FROM t WHERE s LIKE '{"b":1, %'"#,
            r#"SELECT j ->> 'k' FROM t WHERE j = '{"a":1}'"#,
            "SELECT 1 FROM t WHERE j = 'not json'",
        ] {
            assert_eq!(jsonb_literals(sql, &s).as_deref(), Ok(sql), "{sql}");
        }
        // No exact spelling, an order DuckDB does not share, or a place it may meet a jsonb.
        for sql in [
            r#"SELECT 1 FROM t WHERE j = '{"a":1.0}'"#,
            r#"SELECT 1 FROM t WHERE j = $$ {"b":1, "a":2} $$"#,
            r#"SELECT 1 FROM t WHERE j < '{"a":1}'"#,
            r#"SELECT 1 FROM t WHERE j = coalesce(NULL, '{"b":1, "a":2}')"#,
            r#"SELECT 1 FROM t WHERE nope = '{"b":1, "a":2}'"#,
        ] {
            assert!(jsonb_literals(sql, &s).is_err(), "{sql}");
        }
    }
}
