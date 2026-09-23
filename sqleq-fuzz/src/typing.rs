//! What each `$N` has to *be*, read off the syntax that surrounds it.
//!
//! [`crate::patterns::param_cols`] learns a parameter's type from one shape — a bare column directly
//! across a comparison operator — and every parameter it cannot place falls back to an integer. That
//! fallback is visible in DuckDB's own words: `INTEGER_LITERAL` as an operand or argument type means
//! nothing was known about the slot, and the pair reports a bind error instead of a verdict. This
//! module recovers the rest of the shapes from the parse tree:
//!
//!   * a **function argument** whose type the function fixes — `upper($1)` is text, `date_trunc($1, x)`
//!     is a field name, `x AT TIME ZONE $1` is a zone name, `j -> $1` is a JSON key;
//!   * the **peer** of a comparison, `BETWEEN`, or `IN` list, when that peer is any expression that
//!     types rather than only a bare column — a cast, a `COALESCE` over columns, a call;
//!   * a `LIKE`/`ILIKE` **pattern**, which is text whatever sits on the left.
//!
//! Three properties keep this safe to consult.
//!
//! It is **strictly additive**: [`crate::pair`] asks for a need only where it had no column evidence at
//! all, so a slot that already resolved is untouched and no existing verdict can move for this reason.
//!
//! It is **pair-level**, like `param_cols` before it: both queries are read into one map and the bind
//! is a single value per `$N` used on both sides. A wrong guess can therefore cost a verdict but cannot
//! forge one — the comparison is still two sorted multisets produced under identical bindings.
//!
//! And it **declines rather than guesses**. Every rule here is one where the syntax fixes the type;
//! nothing infers a type from a name, a position, or a frequency. A slot no rule reaches keeps the
//! integer fallback it already had.

use std::collections::HashMap;
use std::ops::ControlFlow;

use sqlparser::ast::{
    BinaryOperator, Expr, FunctionArg, FunctionArgExpr, FunctionArguments, Query, Select,
    SelectItem, SetExpr, Statement, Value, Visit, Visitor,
};
use sqlparser::dialect::PostgreSqlDialect;
use sqlparser::parser::Parser;

use crate::gen::{cast_target, CastTarget};
use crate::schema::VType;

/// What a `$N` has to be for the statement to bind.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Need {
    /// A value from this type's generation domain.
    Type(CastTarget),
    /// A time-zone name (`x AT TIME ZONE $N`, `timezone($N, x)`). Not any string: DuckDB rejects an
    /// unknown zone outright, so the domain is a handful of real IANA names.
    TimeZone,
    /// A date/time field name (`date_trunc($N, x)`, `date_part($N, x)`) — likewise a closed set.
    DateField,
    /// A JSON object key (`j -> $N`, `j ->> $N`).
    JsonKey,
    /// A `LIKE`/`ILIKE` pattern.
    LikePattern,
    /// A fraction in `[0, 1]`: `percentile_cont($N)` and `percentile_disc($N)` reject anything else
    /// at run time, so the type alone is not enough.
    Fraction,
    /// A POSIX regular expression (the right operand of `~`, `~*`, `!~`, `!~*`). A separate domain
    /// from `LikePattern` because the wildcards differ: `%` is a literal per-cent to a regex engine,
    /// so a `LIKE` pattern used here matches nothing and silently costs the comparison its power.
    Regex,
    /// Text that is also a legal integer literal. Used where one side compares `$N` against a
    /// *text* expression and the other casts it to an integer — `c::varchar = $N` against
    /// `c = $N::int4`. Both demands have to be met at once: the value must bind as text on the one
    /// side and survive `::int4` on the other, and it must include a non-canonical spelling, or the
    /// two sides agree by construction and the rewrite's effect is unobservable.
    ///
    /// Integer only, never `Double`/`Numeric`: `1::varchar` is `'1'` in both Postgres and DuckDB, so
    /// the integer case is dialect-safe, while float and numeric rendering is not (`1.50::text`
    /// keeps its scale in Postgres). A rendering difference would be a DuckDB artifact wearing the
    /// costume of a counterexample.
    NumericString,
}

/// The type each `$N` must take, from both queries of the pair.
///
/// `cols` maps a bare column name to its generated type, exactly as [`crate::pair`] resolves it, so
/// the two agree on what a column reference means. Reading A before B makes the result deterministic
/// where the two sides would fix a slot differently; either way both sides receive the same bind.
pub fn param_needs(a: &str, b: &str, cols: &HashMap<String, VType>) -> HashMap<u32, Need> {
    let mut needs = Needs {
        cols: cols
            .iter()
            .map(|(k, v)| (k.clone(), CastTarget::V(*v)))
            .collect(),
        out: HashMap::new(),
        hard: HashMap::new(),
    };
    let mut stmts = Vec::new();
    for sql in [a, b] {
        if let Ok(parsed) = Parser::parse_sql(&PostgreSqlDialect {}, sql) {
            stmts.extend(parsed);
        }
    }

    // A projection alias names an expression, so it types like that expression -- which is how a
    // comparison against a CTE or derived-table column gets a type at all, since no such column is
    // declared in the DDL. A declared column always wins, so an alias that shadows one cannot
    // retype it. Chained aliases (a CTE column named from another CTE's alias) need more than one
    // round; the loop stops as soon as a round adds nothing.
    let mut aliases = Aliases::default();
    for st in &stmts {
        let _ = Statement::visit(st, &mut aliases);
    }
    for _ in 0..3 {
        let mut added = false;
        for (name, expr) in &aliases.0 {
            if needs.cols.contains_key(name) {
                continue;
            }
            if let Some(t) = needs.ty(expr) {
                needs.cols.insert(name.clone(), t);
                added = true;
            }
        }
        if !added {
            break;
        }
    }

    for st in &stmts {
        let _ = Statement::visit(st, &mut needs);
    }
    // A cast outranks everything read off a peer, in either direction and whichever side it is on --
    // except where the two demands cannot both be satisfied by a canonical value. `c::varchar = $N`
    // on one side and `c = $N::int4` on the other is this corpus's "cast the parameter, not the
    // column" rewrite: let the cast win and `$N` binds an integer, whereupon `c::varchar = 1`
    // coerces to precisely the comparison the other side already makes and the difference the
    // rewrite introduces becomes unobservable. `NumericString` satisfies both sides instead.
    let (mut out, hard) = (needs.out, needs.hard);
    for (n, h) in hard {
        let joint = matches!(
            (out.get(&n), h),
            (
                Some(Need::Type(CastTarget::V(VType::Varchar))),
                Need::Type(CastTarget::V(VType::Integer))
            ) | (
                Some(Need::Type(CastTarget::V(VType::Integer))),
                Need::Type(CastTarget::V(VType::Varchar))
            )
        );
        out.insert(n, if joint { Need::NumericString } else { h });
    }
    out
}

/// Every `expr AS alias` in either query, in the order found.
#[derive(Default)]
struct Aliases(Vec<(String, Expr)>);

impl Visitor for Aliases {
    type Break = ();

    fn pre_visit_select(&mut self, select: &Select) -> ControlFlow<()> {
        for item in &select.projection {
            if let SelectItem::ExprWithAlias { expr, alias } = item {
                self.0
                    .push((alias.value.trim_matches('"').to_lowercase(), expr.clone()));
            }
        }
        ControlFlow::Continue(())
    }
}

/// The single expression a scalar subquery projects, so a comparison against it can be typed.
fn only_projection(q: &Query) -> Option<&Expr> {
    let SetExpr::Select(sel) = &*q.body else {
        return None;
    };
    match sel.projection.as_slice() {
        [SelectItem::UnnamedExpr(e)] | [SelectItem::ExprWithAlias { expr: e, .. }] => Some(e),
        _ => None,
    }
}

struct Needs {
    /// Declared columns, plus the projection aliases resolved above.
    cols: HashMap<String, CastTarget>,
    /// Needs read off a peer: what the parameter must be *comparable to*.
    out: HashMap<u32, Need>,
    /// Needs read off a cast over the parameter itself: what the value must *convert to*. A stronger
    /// claim than any peer makes, so it is kept apart and merged over `out` at the end.
    hard: HashMap<u32, Need>,
}

/// `$N` → `N`, for a *bare* placeholder only. A placeholder under a cast is already typed by the cast
/// (see `param_casts`), and a placeholder under anything else is not the slot this rule is about.
fn placeholder(e: &Expr) -> Option<u32> {
    match e {
        Expr::Nested(inner) => placeholder(inner),
        Expr::Value(v) => match &v.value {
            Value::Placeholder(p) => p.strip_prefix('$')?.parse().ok(),
            _ => None,
        },
        _ => None,
    }
}

/// The comparisons whose two sides must share a type. Deliberately just these: a regex or containment
/// operator constrains its operands differently, and `IS`/`IS NOT` take no parameter.
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

/// An `INTERVAL` operand, in any spelling: the literal, a cast to it, or one scaled by a count --
/// `$1 * INTERVAL '1 day'` is an interval, which is what makes `ts + ($1 * $2::interval)` a timestamp
/// without making `$1` anything but the count it is.
fn is_interval(e: &Expr) -> bool {
    match e {
        Expr::Nested(inner) => is_interval(inner),
        Expr::Interval(_) => true,
        Expr::BinaryOp {
            left,
            op: BinaryOperator::Multiply,
            right,
        } => is_interval(left) || is_interval(right),
        Expr::Cast { data_type, .. } => {
            matches!(
                cast_target(&data_type.to_string()),
                Some(CastTarget::Interval)
            )
        }
        _ => false,
    }
}

/// The functions whose argument types are fixed regardless of what is passed. `None` for everything
/// else — an unknown function tells us nothing about its arguments.
fn arg_need(name: &str, idx: usize) -> Option<Need> {
    match name {
        "date_trunc" | "date_part" | "datepart" | "date_diff" | "datediff" if idx == 0 => {
            Some(Need::DateField)
        }
        "timezone" if idx == 0 => Some(Need::TimeZone),
        "percentile_cont" | "percentile_disc" if idx == 0 => Some(Need::Fraction),
        "upper" | "lower" | "ucase" | "lcase" | "initcap" | "md5" | "trim" | "btrim" | "ltrim"
        | "rtrim" | "reverse" | "encode" | "decode" | "unaccent" | "ascii" | "quote_literal" => {
            Some(Need::Type(CastTarget::V(VType::Varchar)))
        }
        "replace"
        | "concat"
        | "concat_ws"
        | "strpos"
        | "position"
        | "instr"
        | "split_part"
        | "string_split"
        | "string_to_array"
        | "starts_with"
        | "ends_with"
        | "contains"
        | "regexp_replace"
        | "regexp_matches"
        | "regexp_full_match"
        | "regexp_like"
        | "regexp_split_to_array"
        | "similar_to"
        | "translate"
        | "repeat"
        | "lpad"
        | "rpad"
        | "char_length"
        | "character_length"
        | "length"
        | "octet_length"
        | "substr"
        | "substring"
        | "left"
        | "right"
        | "to_tsvector"
        | "to_tsquery"
        | "plainto_tsquery"
        | "websearch_to_tsquery" => Some(Need::Type(CastTarget::V(VType::Varchar))),
        // Two-argument `to_timestamp(text, format)` is the text form; the one-argument overload takes
        // epoch seconds, so only the pair is safe to call text -- but the *format* is always text.
        "to_timestamp" | "to_date" | "to_char" | "to_number" if idx == 1 => {
            Some(Need::Type(CastTarget::V(VType::Varchar)))
        }
        "to_timestamp" | "to_date" | "to_char" | "to_number" if idx == 0 => None,
        "log" | "ln" | "exp" | "sqrt" | "power" | "pow" => {
            Some(Need::Type(CastTarget::V(VType::Double)))
        }
        _ => None,
    }
}

/// The type a call *returns*, where the function fixes it. Feeds peer propagation, so that
/// `COALESCE(a, b) >= $1` types `$1` from the columns inside the `COALESCE`.
fn ret_ty(name: &str) -> Option<CastTarget> {
    match name {
        "upper" | "lower" | "ucase" | "lcase" | "initcap" | "md5" | "trim" | "btrim" | "ltrim"
        | "rtrim" | "reverse" | "concat" | "concat_ws" | "replace" | "substr" | "substring"
        | "left" | "right" | "split_part" | "to_char" | "lpad" | "rpad" | "translate" => {
            Some(CastTarget::V(VType::Varchar))
        }
        "length" | "char_length" | "character_length" | "octet_length" | "strpos" | "position"
        | "count" | "row_number" | "rank" | "dense_rank" => Some(CastTarget::V(VType::Integer)),
        "date_trunc" | "to_timestamp" => Some(CastTarget::V(VType::Timestamp)),
        "to_date" => Some(CastTarget::V(VType::Date)),
        "starts_with" | "ends_with" | "contains" | "regexp_full_match" | "regexp_like" => {
            Some(CastTarget::V(VType::Boolean))
        }
        _ => None,
    }
}

/// Functions that pass a type through from their arguments rather than fixing one.
fn is_passthrough(name: &str) -> bool {
    matches!(
        name,
        "coalesce" | "ifnull" | "nullif" | "nvl" | "greatest" | "least" | "min" | "max"
    )
}

impl Needs {
    /// Record a need for `e`, if `e` is a bare placeholder. First writer wins, so the traversal order
    /// (query A, then B; outer expression before inner) decides, deterministically.
    fn want(&mut self, e: &Expr, need: Need) {
        if let Some(n) = placeholder(e) {
            self.out.entry(n).or_insert(need);
        }
    }

    /// Record what a cast over the parameter demands. `$1::uuid` and `($1)::uuid` are the same demand;
    /// [`crate::patterns::param_casts`] only sees the first spelling, and where it does see one this
    /// never gets consulted, so agreeing with it costs nothing and covers the rest.
    fn must(&mut self, e: &Expr, ct: CastTarget) {
        if let Some(n) = placeholder(e) {
            self.hard.entry(n).or_insert(Need::Type(ct));
        }
    }

    fn col(&self, name: &str) -> Option<CastTarget> {
        let key = name.trim_matches('"').to_lowercase();
        self.cols.get(&key).copied()
    }

    /// The type of an expression, where the syntax fixes it. `None` means "no opinion" — never a guess.
    fn ty(&self, e: &Expr) -> Option<CastTarget> {
        match e {
            Expr::Nested(inner) => self.ty(inner),
            Expr::Identifier(i) => self.col(&i.value),
            Expr::CompoundIdentifier(parts) => self.col(&parts.last()?.value),
            Expr::Cast { data_type, .. } => cast_target(&data_type.to_string()),
            Expr::AtTimeZone { .. } => Some(CastTarget::V(VType::Timestamp)),
            Expr::Interval(_) => Some(CastTarget::Interval),
            Expr::IsDistinctFrom(..) | Expr::IsNotDistinctFrom(..) => {
                Some(CastTarget::V(VType::Boolean))
            }
            // A scalar subquery types as the one expression it projects.
            Expr::Subquery(q) => only_projection(q).and_then(|e| self.ty(e)),
            // Every branch of a `CASE` shares one type, so the first branch with an opinion gives it.
            Expr::Case {
                conditions,
                else_result,
                ..
            } => conditions
                .iter()
                .map(|w| &w.result)
                .chain(else_result.iter().map(|b| &**b))
                .find_map(|e| self.ty(e)),
            Expr::BinaryOp { left, op, right } => match op {
                BinaryOperator::StringConcat => Some(CastTarget::V(VType::Varchar)),
                BinaryOperator::And | BinaryOperator::Or => Some(CastTarget::V(VType::Boolean)),
                op if is_comparison(op) => Some(CastTarget::V(VType::Boolean)),
                // `ts + INTERVAL '1 day'` is a timestamp: whichever operand is not the interval
                // decides, and two operands that are both plain numbers decide nothing.
                BinaryOperator::Plus | BinaryOperator::Minus => {
                    if is_interval(right) {
                        self.ty(left)
                    } else if is_interval(left) {
                        self.ty(right)
                    } else {
                        None
                    }
                }
                _ => None,
            },
            Expr::Value(v) => match &v.value {
                Value::SingleQuotedString(_) | Value::DollarQuotedString(_) => {
                    Some(CastTarget::V(VType::Varchar))
                }
                Value::Number(_, _) => None, // an integer literal is what we are trying to get away from
                Value::Boolean(_) => Some(CastTarget::V(VType::Boolean)),
                _ => None,
            },
            Expr::Function(f) => {
                let name = f.name.to_string().rsplit('.').next()?.to_lowercase();
                if is_passthrough(&name) {
                    // The common type of the arguments: the first one that has an opinion.
                    if let FunctionArguments::List(list) = &f.args {
                        return list.args.iter().find_map(|a| match a {
                            FunctionArg::Unnamed(FunctionArgExpr::Expr(x))
                            | FunctionArg::Named {
                                arg: FunctionArgExpr::Expr(x),
                                ..
                            } => self.ty(x),
                            _ => None,
                        });
                    }
                    return None;
                }
                ret_ty(&name)
            }
            _ => None,
        }
    }
}

impl Visitor for Needs {
    type Break = ();

    fn pre_visit_expr(&mut self, e: &Expr) -> ControlFlow<()> {
        match e {
            // ---- positions whose type the construct fixes ----
            Expr::AtTimeZone { time_zone, .. } => self.want(time_zone, Need::TimeZone),
            Expr::Like { pattern, .. }
            | Expr::ILike { pattern, .. }
            | Expr::SimilarTo { pattern, .. } => self.want(pattern, Need::LikePattern),
            Expr::Function(f) => {
                if let FunctionArguments::List(list) = &f.args {
                    let name = f
                        .name
                        .to_string()
                        .rsplit('.')
                        .next()
                        .unwrap_or_default()
                        .to_lowercase();
                    for (i, a) in list.args.iter().enumerate() {
                        let arg = match a {
                            FunctionArg::Unnamed(FunctionArgExpr::Expr(x))
                            | FunctionArg::Named {
                                arg: FunctionArgExpr::Expr(x),
                                ..
                            } => x,
                            _ => continue,
                        };
                        if let Some(need) = arg_need(&name, i) {
                            self.want(arg, need);
                        } else if is_passthrough(&name) {
                            // `COALESCE(col, $1)`: the parameter is a peer of the other arguments.
                            if let Some(t) = list.args.iter().find_map(|o| match o {
                                FunctionArg::Unnamed(FunctionArgExpr::Expr(x))
                                | FunctionArg::Named {
                                    arg: FunctionArgExpr::Expr(x),
                                    ..
                                } => self.ty(x),
                                _ => None,
                            }) {
                                self.want(arg, Need::Type(t));
                            }
                        }
                    }
                }
            }

            // ---- the parameter's own type, stated outright ----
            Expr::Cast {
                expr, data_type, ..
            } => {
                if let Some(ct) = cast_target(&data_type.to_string()) {
                    self.must(expr, ct);
                }
            }

            // Both operands of a concatenation are text, and both operands of `AND`/`OR` are
            // booleans. Only a bare `$N` in those positions is affected; a real subexpression there
            // is not a placeholder and `want` passes over it.
            Expr::IsDistinctFrom(l, r) | Expr::IsNotDistinctFrom(l, r) => {
                if let Some(t) = self.ty(l) {
                    self.want(r, Need::Type(t));
                }
                if let Some(t) = self.ty(r) {
                    self.want(l, Need::Type(t));
                }
            }
            Expr::Case {
                operand,
                conditions,
                else_result,
                ..
            } => {
                let branches: Vec<&Expr> = conditions
                    .iter()
                    .map(|w| &w.result)
                    .chain(else_result.iter().map(|b| &**b))
                    .collect();
                if let Some(t) = branches.iter().find_map(|e| self.ty(e)) {
                    for e in &branches {
                        self.want(e, Need::Type(t));
                    }
                }
                // `CASE x WHEN $1 THEN ...` compares each condition against the operand instead.
                if let Some(x) = operand {
                    if let Some(t) = self.ty(x) {
                        for w in conditions {
                            self.want(&w.condition, Need::Type(t));
                        }
                    }
                }
            }

            // ---- positions typed by their peer ----
            Expr::Between {
                expr, low, high, ..
            } => {
                if let Some(t) = self.ty(expr) {
                    self.want(low, Need::Type(t));
                    self.want(high, Need::Type(t));
                }
            }
            Expr::InList { expr, list, .. } => {
                if let Some(t) = self.ty(expr) {
                    for x in list {
                        self.want(x, Need::Type(t));
                    }
                }
            }
            Expr::BinaryOp { left, op, right } => match op {
                BinaryOperator::Arrow | BinaryOperator::LongArrow => {
                    self.want(right, Need::JsonKey)
                }
                BinaryOperator::StringConcat => {
                    self.want(left, Need::Type(CastTarget::V(VType::Varchar)));
                    self.want(right, Need::Type(CastTarget::V(VType::Varchar)));
                }
                BinaryOperator::And | BinaryOperator::Or => {
                    self.want(left, Need::Type(CastTarget::V(VType::Boolean)));
                    self.want(right, Need::Type(CastTarget::V(VType::Boolean)));
                }
                BinaryOperator::PGRegexMatch
                | BinaryOperator::PGRegexIMatch
                | BinaryOperator::PGRegexNotMatch
                | BinaryOperator::PGRegexNotIMatch => self.want(right, Need::Regex),
                op if is_comparison(op) => {
                    if let Some(t) = self.ty(left) {
                        self.want(right, Need::Type(t));
                    }
                    if let Some(t) = self.ty(right) {
                        self.want(left, Need::Type(t));
                    }
                }
                // `$1 - INTERVAL '1 day'`: only the interval spelling fixes the other operand as a
                // timestamp. `ts + ($1 * $2::interval)` deliberately does not — there `$1` is a count.
                BinaryOperator::Minus | BinaryOperator::Plus => {
                    if is_interval(right) {
                        self.want(left, Need::Type(CastTarget::V(VType::Timestamp)));
                    }
                    if is_interval(left) {
                        self.want(right, Need::Type(CastTarget::V(VType::Timestamp)));
                    }
                }
                _ => {}
            },
            _ => {}
        }
        ControlFlow::Continue(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cols() -> HashMap<String, VType> {
        [
            ("ts", VType::Timestamp),
            ("ts2", VType::Timestamp),
            ("n", VType::Integer),
            ("s", VType::Varchar),
            ("u", VType::Uuid),
            ("j", VType::Varchar),
        ]
        .into_iter()
        .map(|(k, v)| (k.to_string(), v))
        .collect()
    }

    fn need(sql: &str, n: u32) -> Option<Need> {
        param_needs(sql, sql, &cols()).get(&n).copied()
    }
    fn ty(v: VType) -> Option<Need> {
        Some(Need::Type(CastTarget::V(v)))
    }

    /// A function fixes its argument's type even though nothing nearby is a column.
    #[test]
    fn a_function_argument_is_typed_by_the_function() {
        assert_eq!(
            need("SELECT 1 FROM t WHERE upper(s) = upper($1)", 1),
            ty(VType::Varchar)
        );
        assert_eq!(
            need("SELECT 1 FROM t WHERE ts >= date_trunc($1, ts2)", 1),
            Some(Need::DateField)
        );
        assert_eq!(need("SELECT lower($1) FROM t", 1), ty(VType::Varchar));
    }

    #[test]
    fn a_time_zone_and_a_json_key_are_their_own_domains() {
        assert_eq!(
            need("SELECT (ts AT TIME ZONE $1)::date FROM t", 1),
            Some(Need::TimeZone)
        );
        assert_eq!(
            need("SELECT timezone($1, ts) FROM t", 1),
            Some(Need::TimeZone)
        );
        assert_eq!(
            need("SELECT 1 FROM t WHERE j -> $1 IS NOT NULL", 1),
            Some(Need::JsonKey)
        );
        assert_eq!(
            need("SELECT 1 FROM t WHERE j ->> $1 = $2", 1),
            Some(Need::JsonKey)
        );
    }

    /// A `LIKE` pattern is text whatever sits on the left — including the expression forms that leave
    /// the column-evidence rule with nothing to match.
    #[test]
    fn a_like_pattern_is_text_whatever_it_is_matched_against() {
        assert_eq!(
            need("SELECT 1 FROM t WHERE s LIKE $1", 1),
            Some(Need::LikePattern)
        );
        assert_eq!(
            need("SELECT 1 FROM t WHERE j ->> $1 LIKE $2", 2),
            Some(Need::LikePattern)
        );
        assert_eq!(
            need("SELECT 1 FROM t WHERE upper(s::text) ILIKE $1", 1),
            Some(Need::LikePattern)
        );
    }

    /// The peer of a comparison types the parameter even when that peer is an expression rather than a
    /// bare column: a `COALESCE` over columns, a cast, a call.
    #[test]
    fn a_parameter_is_typed_by_its_peer_expression() {
        assert_eq!(
            need("SELECT 1 FROM t WHERE COALESCE(ts, ts2) >= $1", 1),
            ty(VType::Timestamp)
        );
        assert_eq!(
            need("SELECT 1 FROM t WHERE n::uuid = $1", 1),
            ty(VType::Uuid)
        );
        assert_eq!(
            need("SELECT 1 FROM t WHERE $1 = COALESCE(s, 'x')", 1),
            ty(VType::Varchar)
        );
        assert_eq!(
            need("SELECT 1 FROM t WHERE COALESCE(ts, $1) IS NULL", 1),
            ty(VType::Timestamp)
        );
    }

    #[test]
    fn between_and_in_take_the_left_operands_type() {
        let m = param_needs("SELECT 1 FROM t WHERE ts BETWEEN $1 AND $2", "", &cols());
        assert_eq!(m.get(&1).copied(), ty(VType::Timestamp));
        assert_eq!(m.get(&2).copied(), ty(VType::Timestamp));
        let m = param_needs("SELECT 1 FROM t WHERE u IN ($1, $2)", "", &cols());
        assert_eq!(m.get(&1).copied(), ty(VType::Uuid));
        assert_eq!(m.get(&2).copied(), ty(VType::Uuid));
    }

    #[test]
    fn an_interval_operand_makes_its_peer_temporal() {
        assert_eq!(
            need("SELECT 1 FROM t WHERE ($1 - INTERVAL '1 day') > ts", 1),
            ty(VType::Timestamp)
        );
        assert_eq!(
            need("SELECT 1 FROM t WHERE ($1 - $2::interval) > ts", 1),
            ty(VType::Timestamp)
        );
    }

    /// The rules decline wherever the syntax does not actually fix a type. These are the cases a
    /// looser pass would get wrong, and getting them wrong costs a verdict rather than merely failing
    /// to gain one.
    #[test]
    fn nothing_is_guessed_where_the_syntax_says_nothing() {
        // A multiplier in front of an interval is a count, not a timestamp.
        assert_eq!(
            need("SELECT 1 FROM t WHERE ts < ts2 + ($1 * $2::interval)", 1),
            None
        );
        // An unknown function tells us nothing about its arguments.
        assert_eq!(need("SELECT 1 FROM t WHERE some_udf($1) = n", 1), None);
        // Neither does a comparison against an unknown column or a bare number.
        assert_eq!(need("SELECT 1 FROM t WHERE unknown_col = $1", 1), None);
        assert_eq!(need("SELECT 1 FROM t WHERE 5 = $1", 1), None);
        // `to_timestamp` has two overloads with different first-argument types, so it declines.
        assert_eq!(need("SELECT to_timestamp($1) FROM t", 1), None);
    }

    /// A cast over the parameter states its type outright, so it beats anything a peer implies — in
    /// either order, on either side, and through the parenthesised spelling `param_casts` cannot see.
    /// Binding one value per `$N` is what makes this matter: the value has to satisfy the cast.
    ///
    /// One conflict is not settled this way — a *text* peer against an integer cast, where obeying
    /// the cast alone would erase the very difference the pair is being tested for. See
    /// [`numeric_string_resolves_a_text_int_conflict`] and [`Need::NumericString`].
    #[test]
    fn a_cast_over_the_parameter_outranks_its_peer() {
        // A peer the cast disagrees with still loses to it, in either order and on either side.
        let a = "SELECT 1 FROM t WHERE ts = $1";
        let b = "SELECT 1 FROM t WHERE n = ($1)::integer";
        let int = ty(VType::Integer);
        assert_eq!(
            param_needs(a, b, &cols()).get(&1).copied(),
            int,
            "cast wins from B"
        );
        assert_eq!(
            param_needs(b, a, &cols()).get(&1).copied(),
            int,
            "and from A"
        );
        assert_eq!(need("SELECT 1 FROM t WHERE s = $1::integer", 1), int);
        assert_eq!(
            need("SELECT 1 FROM t WHERE upper(s) = ($1)::uuid", 1),
            ty(VType::Uuid)
        );
    }

    /// The carve-out, and the reason it exists. `n::text = $1` against `n = $1::integer` is the
    /// corpus's "cast the parameter, not the column" rewrite. Obey the cast and `$1` binds a
    /// canonical integer, whereupon `n::text = 1` coerces to precisely the comparison the other side
    /// already makes and the rewrite becomes unobservable. Both orders, because which query states
    /// which half of the conflict is an accident of how the corpus row was recorded.
    #[test]
    fn numeric_string_resolves_a_text_int_conflict() {
        let a = "SELECT 1 FROM t WHERE n::text = $1";
        let b = "SELECT 1 FROM t WHERE n = ($1)::integer";
        let ns = Some(Need::NumericString);
        assert_eq!(param_needs(a, b, &cols()).get(&1).copied(), ns, "from B");
        assert_eq!(
            param_needs(b, a, &cols()).get(&1).copied(),
            ns,
            "and from A"
        );
    }

    /// A comparison peer is often not a column but an expression built from one. Each of these is a
    /// shape the corpus actually compares a parameter against.
    #[test]
    fn an_expression_peer_types_as_what_it_computes() {
        // `ts + interval` is a timestamp -- including when the interval is scaled by a count, which
        // is the shape that leaves the count itself untyped.
        assert_eq!(
            need("SELECT 1 FROM t WHERE (ts + INTERVAL '1 day') <= $1", 1),
            ty(VType::Timestamp)
        );
        assert_eq!(
            need("SELECT 1 FROM t WHERE (ts + ($1 * $2::interval)) <= $3", 3),
            ty(VType::Timestamp)
        );
        // A scalar subquery types as the one expression it projects.
        assert_eq!(
            need("SELECT 1 FROM t WHERE (SELECT ts FROM t LIMIT 1) = $1", 1),
            ty(VType::Timestamp)
        );
        // A `CASE` types as its branches, and a simple `CASE` compares its operand to each `WHEN`.
        assert_eq!(
            need("SELECT CASE WHEN n > 0 THEN ts ELSE $1 END FROM t", 1),
            ty(VType::Timestamp)
        );
        assert_eq!(
            need("SELECT CASE s WHEN $1 THEN 1 ELSE 0 END FROM t", 1),
            ty(VType::Varchar)
        );
    }

    /// Operators that fix their operands outright, whatever sits on the other side.
    #[test]
    fn an_operator_can_fix_its_operands_by_itself() {
        assert_eq!(need("SELECT s || $1 FROM t", 1), ty(VType::Varchar));
        assert_eq!(
            need("SELECT 1 FROM t WHERE $1 AND n > 0", 1),
            ty(VType::Boolean)
        );
        assert_eq!(
            need("SELECT 1 FROM t WHERE s IS DISTINCT FROM $1", 1),
            ty(VType::Varchar)
        );
        assert_eq!(need("SELECT 1 FROM t WHERE s ~ $1", 1), Some(Need::Regex));
        assert_eq!(need("SELECT to_char(ts, $1) FROM t", 1), ty(VType::Varchar));
        assert_eq!(
            need(
                "SELECT percentile_cont($1) WITHIN GROUP (ORDER BY n) FROM t",
                1
            ),
            Some(Need::Fraction)
        );
    }

    /// A CTE or derived-table column is declared nowhere, so without the alias pass a comparison
    /// against one types nothing. A declared column still wins, so an alias cannot retype one.
    #[test]
    fn a_projection_alias_types_like_the_expression_it_names() {
        let q =
            "WITH c AS (SELECT ts AS t2, n AS m FROM t) SELECT 1 FROM c WHERE t2 > $1 AND m = $2";
        let m = param_needs(q, "", &cols());
        assert_eq!(m.get(&1).copied(), ty(VType::Timestamp));
        assert_eq!(m.get(&2).copied(), ty(VType::Integer));
        // Chained: `t3` names `t2`, which names a column.
        let q = "WITH c AS (SELECT ts AS t2 FROM t), d AS (SELECT t2 AS t3 FROM c) SELECT 1 FROM d WHERE t3 > $1";
        assert_eq!(
            param_needs(q, "", &cols()).get(&1).copied(),
            ty(VType::Timestamp)
        );
        // Shadowing: `s` is a declared VARCHAR, so an alias of that name does not make it a timestamp.
        let q = "WITH c AS (SELECT ts AS s FROM t) SELECT 1 FROM c WHERE s = $1";
        assert_eq!(
            param_needs(q, "", &cols()).get(&1).copied(),
            ty(VType::Varchar)
        );
    }

    /// Reading both queries is what makes the map pair-level: a slot only one side constrains is still
    /// bound from that constraint, and A is read first so a disagreement resolves deterministically.
    #[test]
    fn both_queries_contribute_and_a_wins() {
        let a = "SELECT 1 FROM t WHERE ts = $1";
        let b = "SELECT 1 FROM t WHERE upper(s) = $1 AND j -> $2 IS NOT NULL";
        let m = param_needs(a, b, &cols());
        assert_eq!(
            m.get(&1).copied(),
            ty(VType::Timestamp),
            "A's constraint wins on $1"
        );
        assert_eq!(
            m.get(&2).copied(),
            Some(Need::JsonKey),
            "B alone constrains $2"
        );
        let m = param_needs(b, a, &cols());
        assert_eq!(
            m.get(&1).copied(),
            ty(VType::Varchar),
            "with B first, B's constraint wins"
        );
    }
}
