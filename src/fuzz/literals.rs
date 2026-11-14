// Unless explicitly stated otherwise all files in this repository are licensed under the
// Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/).
// Copyright 2026-Present Datadog, Inc.

//! The literals a pair compares its columns with, as values a column can hold.
//!
//! The generated domains are small and fixed ([`crate::fuzz::gen`]), so a comparison with a literal
//! outside them, `334 <= c`, holds for every generated row or for none, and a pair that differs only
//! at that literal's boundary (`334 <= c` against `334 < c`) agrees on every trial. A boundary change
//! is among the commonest wrong rewrites, and it can be told apart only by a row that holds the
//! literal itself, or, for an integer or a date, the value next to it. These are those values, for
//! the trials that draw them ([`crate::fuzz::pg`]).
//!
//! There are two readings. [`harvest`] takes a literal a column itself is compared with, as the
//! text the query writes. [`harvest_wide`] also takes one compared with an expression of the column
//! (`coalesce(c, 0) <= -2`), one a `CASE c WHEN` holds its operand against, and a constant that is
//! not its literal's text (`CAST('-1.7305e-12'::float8 AS int8)` is `0`), which the engine is asked
//! for. They are kept apart because the trials that draw the first reading's values must go on
//! drawing exactly those.

use std::collections::BTreeMap;
use std::ops::ControlFlow;

use sqlparser::ast::{BinaryOperator, Expr, InTarget, Query, UnaryOperator, Value, Visit, Visitor};
use sqlparser::dialect::PostgreSqlDialect;
use sqlparser::parser::Parser;

use crate::fuzz::gather::{kind, Kind};
use crate::fuzz::gen::Val;
use crate::fuzz::schema::VType;

/// A literal as the query writes it: a number, or the text of a string (whatever it is cast to).
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Lit {
    Number(String),
    Text(String),
}

/// Each column a comparison, a `BETWEEN`, an `IN` list or an `IS [NOT] DISTINCT FROM` in either
/// statement holds against a literal, by its bare lower-cased name, and the literals.
///
/// A column reference counts under parentheses or a cast, a literal under a cast or a unary minus, and
/// a typed literal (`DATE '2000-01-01'`) as its text. A parameter is no literal: its value is drawn
/// already, and biased toward what a column holds. A name stands for every column of that name in any
/// table: a literal added to a column it is not compared with only adds a value to its domain.
pub fn harvest(a: &str, b: &str) -> BTreeMap<String, Vec<Lit>> {
    let mut h = Harvest(BTreeMap::new());
    for sql in [a, b] {
        if let Ok(parsed) = Parser::parse_sql(&PostgreSqlDialect {}, sql) {
            for st in &parsed {
                let _ = st.visit(&mut h);
            }
        }
    }
    h.0
}

/// What [`harvest_wide`] finds a column held against.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Side {
    /// A literal, as [`harvest`] reads one.
    Lit(Lit),
    /// A constant that need not be its literal's text, as SQL: a cast over a literal, or arithmetic
    /// over literals. What it is, is the engine's to say.
    Constant(String),
}

/// [`harvest`], read more widely: each column that a comparison, a `BETWEEN`, an `IN` list or an
/// `IS [NOT] DISTINCT FROM` in either statement holds against a constant, by its bare lower-cased
/// name, and the constants.
///
/// - **The column may be anywhere under its side**: in `coalesce(c, 0) <= -2` it is `c`, in
///   `a + b > 10` both. A column inside a subquery is not under it; that is another row's.
/// - **A `CASE c WHEN k` holds `c` against `k`.**
/// - **A constant is a literal, or literals under casts, signs, arithmetic and parentheses.** One
///   with a cast or an operator in it is given as SQL too ([`Side::Constant`]), since its value can
///   be another than the text inside it: `CAST('-1.7305e-12'::float8 AS int8)` is `0`.
///
/// Everything [`harvest`] finds is here as a [`Side::Lit`], so this reading only adds. A name
/// stands for every column of that name, as there, and a constant of another type than the column
/// is dropped when the column's values are made ([`values`]).
pub fn harvest_wide(a: &str, b: &str) -> BTreeMap<String, Vec<Side>> {
    let mut h = Wide(BTreeMap::new());
    for sql in [a, b] {
        if let Ok(parsed) = Parser::parse_sql(&PostgreSqlDialect {}, sql) {
            for st in &parsed {
                let _ = st.visit(&mut h);
            }
        }
    }
    h.0
}

struct Wide(BTreeMap<String, Vec<Side>>);

impl Wide {
    fn add(&mut self, col: &Expr, constant: &Expr) {
        let sides = sides(constant);
        if sides.is_empty() {
            return;
        }
        for c in columns(col) {
            let held = self.0.entry(c).or_default();
            for s in &sides {
                if !held.contains(s) {
                    held.push(s.clone());
                }
            }
        }
    }
}

impl Visitor for Wide {
    type Break = ();

    fn pre_visit_expr(&mut self, e: &Expr) -> ControlFlow<()> {
        compared(e, |x, y| self.add(x, y));
        if let Expr::Case { operand: Some(operand), conditions, .. } = e {
            for when in conditions {
                self.add(operand, &when.condition);
            }
        }
        ControlFlow::Continue(())
    }
}

/// Each (column side, literal side) that `e` itself compares, both ways round where either operand
/// could be the column.
fn compared(e: &Expr, mut f: impl FnMut(&Expr, &Expr)) {
    match e {
        Expr::BinaryOp { left, op, right } if is_comparison(op) => {
            f(left, right);
            f(right, left);
        }
        Expr::IsDistinctFrom(x, y) | Expr::IsNotDistinctFrom(x, y) => {
            f(x, y);
            f(y, x);
        }
        Expr::Between { expr, low, high, .. } => {
            f(expr, low);
            f(expr, high);
        }
        Expr::In { expr, target: InTarget::List(list), .. } => {
            for l in list {
                f(expr, l);
            }
        }
        _ => {}
    }
}

struct Harvest(BTreeMap<String, Vec<Lit>>);

impl Harvest {
    fn add(&mut self, col: &Expr, lit: &Expr) {
        if let (Some(c), Some(l)) = (column(col), literal(lit)) {
            let lits = self.0.entry(c).or_default();
            if !lits.contains(&l) {
                lits.push(l);
            }
        }
    }

}

impl Visitor for Harvest {
    type Break = ();

    fn pre_visit_expr(&mut self, e: &Expr) -> ControlFlow<()> {
        compared(e, |x, y| self.add(x, y));
        ControlFlow::Continue(())
    }
}

fn is_comparison(op: &BinaryOperator) -> bool {
    matches!(
        op,
        BinaryOperator::Eq
            | BinaryOperator::NotEq
            | BinaryOperator::Lt
            | BinaryOperator::LtEq
            | BinaryOperator::Gt
            | BinaryOperator::GtEq
    )
}

/// The bare lower-cased name of a column reference, looking through parentheses and casts.
fn column(e: &Expr) -> Option<String> {
    match e {
        Expr::Identifier(id) => Some(id.value.to_lowercase()),
        Expr::CompoundIdentifier(parts) => parts.last().map(|id| id.value.to_lowercase()),
        Expr::Nested(x) | Expr::Cast { expr: x, .. } => column(x),
        _ => None,
    }
}

/// The bare lower-cased names of the columns under `e`, outside any subquery, in reading order.
fn columns(e: &Expr) -> Vec<String> {
    struct Columns {
        depth: usize,
        names: Vec<String>,
    }
    impl Visitor for Columns {
        type Break = ();

        fn pre_visit_query(&mut self, _: &Query) -> ControlFlow<()> {
            self.depth += 1;
            ControlFlow::Continue(())
        }

        fn post_visit_query(&mut self, _: &Query) -> ControlFlow<()> {
            self.depth -= 1;
            ControlFlow::Continue(())
        }

        fn pre_visit_expr(&mut self, e: &Expr) -> ControlFlow<()> {
            let name = match e {
                Expr::Identifier(id) => Some(id.value.to_lowercase()),
                Expr::CompoundIdentifier(parts) => parts.last().map(|id| id.value.to_lowercase()),
                _ => None,
            };
            if let (0, Some(n)) = (self.depth, name) {
                if !self.names.contains(&n) {
                    self.names.push(n);
                }
            }
            ControlFlow::Continue(())
        }
    }
    let mut c = Columns { depth: 0, names: Vec::new() };
    let _ = e.visit(&mut c);
    c.names
}

/// What a constant `e` gives a column: its literal as [`literal`] reads it, and itself as SQL where
/// it computes something.
fn sides(e: &Expr) -> Vec<Side> {
    let mut out: Vec<Side> = literal(e).map(Side::Lit).into_iter().collect();
    if constant(e) && computes(e) {
        out.push(Side::Constant(e.to_string()));
    }
    out
}

/// Whether `e` is literals alone, under casts, signs, arithmetic and parentheses. A parameter, a
/// column, a function and a subquery are none of these: such an expression is not one value, or
/// not one the engine can be asked for ahead of the trials.
fn constant(e: &Expr) -> bool {
    match e {
        Expr::Value(v) => matches!(v.value, Value::Number(..) | Value::SingleQuotedString(_)),
        Expr::TypedString(_) => true,
        Expr::Nested(x) | Expr::Cast { expr: x, .. } => constant(x),
        Expr::UnaryOp { op: UnaryOperator::Minus | UnaryOperator::Plus, expr } => constant(expr),
        Expr::BinaryOp { left, op, right } => is_arithmetic(op) && constant(left) && constant(right),
        _ => false,
    }
}

/// Whether a constant has a cast or an operator between two operands in it, so that its value can
/// be another than the text of the literal inside.
fn computes(e: &Expr) -> bool {
    match e {
        Expr::Cast { .. } | Expr::BinaryOp { .. } => true,
        Expr::Nested(x) | Expr::UnaryOp { expr: x, .. } => computes(x),
        _ => false,
    }
}

fn is_arithmetic(op: &BinaryOperator) -> bool {
    matches!(
        op,
        BinaryOperator::Plus
            | BinaryOperator::Minus
            | BinaryOperator::Multiply
            | BinaryOperator::Divide
            | BinaryOperator::Modulo
            | BinaryOperator::StringConcat
    )
}

fn literal(e: &Expr) -> Option<Lit> {
    match e {
        Expr::Value(v) => match &v.value {
            Value::Number(n, _) => Some(Lit::Number(n.clone())),
            Value::SingleQuotedString(s) => Some(Lit::Text(s.clone())),
            _ => None,
        },
        Expr::UnaryOp { op: UnaryOperator::Minus, expr } => match literal(expr)? {
            Lit::Number(n) if !n.starts_with('-') => Some(Lit::Number(format!("-{n}"))),
            _ => None,
        },
        Expr::Nested(x) | Expr::Cast { expr: x, .. } => literal(x),
        Expr::TypedString(t) => match &t.value.value {
            Value::SingleQuotedString(s) => Some(Lit::Text(s.clone())),
            _ => None,
        },
        _ => None,
    }
}

/// The values a column of domain `vt`, of the Postgres type `ty`, takes from the literals it is
/// compared with: an integer `k` gives `k - 1`, `k` and `k + 1` within the type's range, a date one
/// day either side as well, and any other literal its own text, which Postgres reads as the column's
/// type exactly. A literal the type cannot read gives nothing here, or a row Postgres rejects. A
/// boolean, a json or an array column takes none, and nor does a number written for a string column.
pub fn values(vt: VType, ty: &str, lits: &[Lit]) -> Vec<Val> {
    let mut out: Vec<Val> = Vec::new();
    let mut push = |v: Val| {
        if !out.contains(&v) {
            out.push(v);
        }
    };
    for l in lits {
        let text = match l {
            Lit::Number(n) | Lit::Text(n) => n.trim(),
        };
        match vt {
            VType::Integer => {
                let Ok(k) = text.parse::<i128>() else { continue };
                let max = match kind(ty) {
                    Kind::Int(max) => max as i128,
                    _ => i64::MAX as i128,
                };
                for v in [k - 1, k, k + 1] {
                    // An integer type's range is one wider below than above.
                    if (-max - 1..=max).contains(&v) {
                        push(Val::Int(v as i64));
                    }
                }
            }
            VType::Date => {
                let Some(d) = days(text) else { continue };
                for v in [d - 1, d, d + 1] {
                    push(Val::Date(date(v)));
                }
            }
            VType::Boolean | VType::Json => {}
            VType::Varchar if matches!(l, Lit::Number(_)) => {}
            VType::Uuid => push(Val::Uuid(text.to_string())),
            _ => push(Val::Str(text.to_string())),
        }
    }
    out
}

/// Days since 1970-01-01 of a `YYYY-MM-DD` date (the proleptic Gregorian calendar Postgres uses).
fn days(s: &str) -> Option<i64> {
    let mut parts = s.splitn(3, '-');
    let y: i64 = parts.next()?.parse().ok()?;
    let m: i64 = parts.next()?.parse().ok()?;
    let d: i64 = parts.next()?.parse().ok()?;
    if !(1..=12).contains(&m) || !(1..=31).contains(&d) || !(1..=9999).contains(&y) {
        return None;
    }
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * (m + if m > 2 { -3 } else { 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    Some(era * 146_097 + doe - 719_468)
}

/// The `YYYY-MM-DD` date `z` days after 1970-01-01.
fn date(z: i64) -> String {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + if m <= 2 { 1 } else { 0 };
    format!("{y:04}-{m:02}-{d:02}")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn n(s: &str) -> Lit {
        Lit::Number(s.into())
    }
    fn t(s: &str) -> Lit {
        Lit::Text(s.into())
    }

    #[test]
    fn literals_are_read_off_comparisons_on_either_side() {
        let h = harvest(
            "SELECT * FROM t WHERE 334 <= c AND x.d > '2000-01-01'::date AND e BETWEEN 1 AND -5",
            r#"SELECT * FROM t WHERE (c) < 335 AND f IN ('a', 'b') AND g IS DISTINCT FROM 2.5 AND h = $1 AND "K" = DATE '2001-02-03'"#,
        );
        assert_eq!(h["c"], [n("334"), n("335")]);
        assert_eq!(h["d"], [t("2000-01-01")]);
        assert_eq!(h["e"], [n("1"), n("-5")]);
        assert_eq!(h["f"], [t("a"), t("b")]);
        assert_eq!(h["g"], [n("2.5")]);
        assert_eq!(h["k"], [t("2001-02-03")]);
        assert!(!h.contains_key("h"), "a parameter is no literal");
    }

    #[test]
    fn the_wide_reading_looks_under_an_expression_and_asks_for_a_computed_constant() {
        let c = |s: &str| Side::Constant(s.into());
        let h = harvest_wide(
            "SELECT * FROM t WHERE coalesce(c, 0) <= -2 AND a + x.b > 10 \
               AND (CASE d WHEN '2001-01-13'::date THEN 1 END) = 1 \
               AND e < CAST('-1.7305e-12'::float8 AS int8) AND f = 2 * 3 AND g = $1 + 1 AND h = now() \
               AND i IN (SELECT j FROM u WHERE k = 5) AND length(m) = 0",
            "SELECT * FROM t WHERE c = 7",
        );
        // Under a function, and what the narrow reading finds as well.
        assert_eq!(h["c"], [Side::Lit(n("-2")), Side::Lit(n("7"))]);
        // Every column under the side.
        assert_eq!(h["a"], [Side::Lit(n("10"))]);
        assert_eq!(h["b"], [Side::Lit(n("10"))]);
        // The CASE against 1; and its operand against its arm, whose cast is asked for.
        assert_eq!(
            h["d"],
            [Side::Lit(n("1")), Side::Lit(t("2001-01-13")), c("'2001-01-13'::DATE")]
        );
        // A cast: the literal's text as the narrow reading takes it, and the cast itself.
        assert_eq!(h["e"], [Side::Lit(t("-1.7305e-12")), c("CAST('-1.7305e-12'::FLOAT8 AS INT8)")]);
        // Arithmetic over literals has no literal to read, only a value to ask for.
        assert_eq!(h["f"], [c("2 * 3")]);
        // A parameter and a function are not constants.
        assert!(!h.contains_key("g") && !h.contains_key("h"));
        // A subquery's column is not under the side that holds the subquery; its own comparison is read.
        assert!(!h.contains_key("i") && !h.contains_key("j"));
        assert_eq!(h["k"], [Side::Lit(n("5"))]);
        assert_eq!(h["m"], [Side::Lit(n("0"))]);
    }

    #[test]
    fn the_wide_reading_holds_everything_the_narrow_one_does() {
        let a = "SELECT * FROM t WHERE 334 <= c AND x.d > '2000-01-01'::date AND e BETWEEN 1 AND -5";
        let b = r#"SELECT * FROM t WHERE (c) < 335 AND f IN ('a', 'b') AND g IS DISTINCT FROM 2.5 AND "K" = DATE '2001-02-03'"#;
        let wide = harvest_wide(a, b);
        for (col, lits) in harvest(a, b) {
            for l in lits {
                assert!(wide[&col].contains(&Side::Lit(l.clone())), "{col}: {l:?}");
            }
        }
    }

    #[test]
    fn a_literal_gives_a_column_its_value_and_its_neighbours() {
        assert_eq!(values(VType::Integer, "bigint", &[n("334")]), [Val::Int(333), Val::Int(334), Val::Int(335)]);
        // Never past the type's range.
        assert_eq!(values(VType::Integer, "smallint", &[n("32767")]), [Val::Int(32766), Val::Int(32767)]);
        assert_eq!(values(VType::Integer, "smallint", &[n("-32768")]), [Val::Int(-32768), Val::Int(-32767)]);
        // Not an integer: nothing an integer column could hold.
        assert!(values(VType::Integer, "integer", &[n("2.5")]).is_empty());
        assert_eq!(
            values(VType::Date, "date", &[t("2000-03-01")]),
            [Val::Date("2000-02-29".into()), Val::Date("2000-03-01".into()), Val::Date("2000-03-02".into())]
        );
        assert_eq!(values(VType::Decimal(38, 18), "numeric", &[t("-820")]), [Val::Str("-820".into())]);
        assert_eq!(values(VType::Varchar, "text", &[t("active"), n("1")]), [Val::Str("active".into())]);
        assert!(values(VType::Boolean, "boolean", &[t("true")]).is_empty());
    }

    #[test]
    fn dates_round_trip() {
        for d in ["1970-01-01", "2000-02-29", "1999-12-31", "0001-01-01", "2100-03-01"] {
            assert_eq!(date(days(d).unwrap()), d);
        }
        assert_eq!(days("1970-01-02"), Some(1));
        assert_eq!(days("2000-13-01"), None);
    }
}
