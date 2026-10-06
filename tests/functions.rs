// Unless explicitly stated otherwise all files in this repository are licensed under the
// Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/).
// Copyright 2026-Present Datadog, Inc.

//! Function and aggregate calls: what lowering may read of a call, which calls are aggregates, and
//! which are volatile. Each refusal here is a call that used to lower to the same term as a
//! different call, or to a per-row scalar where Postgres returns one row.

use sqleq_frontend::{lower_sql, FrontendError};

const T: &str = r#"create table "t" ("a" INTEGER, "b" VARCHAR, "c" DOUBLE PRECISION, unique ("a"));
create table "u" ("k" INTEGER, unique ("k"));"#;

/// A pair whose two sides are both `q`, over [`T`].
fn same(q: &str) -> String {
    format!("{T}\n{q};\n{q};")
}

fn ok(sql: &str) -> serde_json::Value {
    lower_sql(sql).unwrap_or_else(|e| panic!("expected Ok, got {e}"))
}

/// Assert `sql` is refused as unsupported, with `needle` in the reason.
fn refused(sql: &str, needle: &str) {
    match lower_sql(sql) {
        Err(FrontendError::Unsupported(m)) => {
            assert!(m.contains(needle), "refused, but for {m:?} rather than {needle:?}")
        }
        Err(e) => panic!("expected an Unsupported refusal mentioning {needle:?}, got {e}"),
        Ok(_) => panic!("expected an Unsupported refusal mentioning {needle:?}, but it lowered: {sql}"),
    }
}

/// The aggregate calls of every `group` node in `v`.
fn group_calls(v: &serde_json::Value) -> Vec<serde_json::Value> {
    let mut out = Vec::new();
    let mut stack = vec![v];
    while let Some(x) = stack.pop() {
        match x {
            serde_json::Value::Object(m) => {
                if let Some(fs) = m.get("group").and_then(|g| g.get("function")).and_then(|f| f.as_array()) {
                    out.extend(fs.iter().cloned());
                }
                stack.extend(m.values());
            }
            serde_json::Value::Array(a) => stack.extend(a.iter()),
            _ => {}
        }
    }
    out
}

/// The aggregate call named `op` in `v`, if a `group` node folds it.
fn group_call(v: &serde_json::Value, op: &str) -> Option<serde_json::Value> {
    group_calls(v).into_iter().find(|c| c["operator"] == op)
}

/// Whether `op` appears anywhere in `v` as an operator.
fn has_op(v: &serde_json::Value, op: &str) -> bool {
    match v {
        serde_json::Value::Object(m) => {
            m.get("operator").and_then(|o| o.as_str()) == Some(op) || m.values().any(|x| has_op(x, op))
        }
        serde_json::Value::Array(a) => a.iter().any(|x| has_op(x, op)),
        _ => false,
    }
}

mod aggregates {
    use super::*;

    /// Every aggregate in Postgres 17's `pg_aggregate`, by name.
    const POSTGRES_AGGREGATES: &[&str] = &[
        "any_value", "array_agg", "avg", "bit_and", "bit_or", "bit_xor", "bool_and", "bool_or",
        "corr", "count", "covar_pop", "covar_samp", "cume_dist", "dense_rank", "every", "json_agg",
        "json_agg_strict", "json_object_agg", "json_object_agg_strict", "json_object_agg_unique",
        "json_object_agg_unique_strict", "jsonb_agg", "jsonb_agg_strict", "jsonb_object_agg",
        "jsonb_object_agg_strict", "jsonb_object_agg_unique", "jsonb_object_agg_unique_strict", "max",
        "min", "mode", "percent_rank", "percentile_cont", "percentile_disc", "range_agg",
        "range_intersect_agg", "rank", "regr_avgx", "regr_avgy", "regr_count", "regr_intercept",
        "regr_r2", "regr_slope", "regr_sxx", "regr_sxy", "regr_syy", "stddev", "stddev_pop",
        "stddev_samp", "string_agg", "sum", "var_pop", "var_samp", "variance", "xmlagg",
        // Not in `pg_aggregate`, but aggregates all the same: the SQL/JSON spellings, and two
        // other dialects' names for `string_agg`.
        "json_arrayagg", "json_objectagg", "group_concat", "listagg",
    ];

    /// The guarantee the lists exist for: no built-in aggregate is lowered as a per-row scalar.
    /// Each call either folds in a `group` node or is refused.
    #[test]
    fn every_postgres_aggregate_is_an_aggregate_or_refused() {
        for f in POSTGRES_AGGREGATES {
            let sql = same(&format!(r#"SELECT {f}("a") FROM "t""#));
            match lower_sql(&sql) {
                Ok(v) => assert!(
                    group_call(&v, &f.to_uppercase()).is_some(),
                    "{f} lowered, but not as an aggregate: {v}"
                ),
                Err(FrontendError::Unsupported(_)) => {}
                Err(e) => panic!("{f}: {e}"),
            }
        }
    }

    #[test]
    fn an_unlisted_aggregate_returns_one_row_under_count() {
        // The issue's pair: on an empty `t` the left side counts one row, the right side none. As a
        // per-row scalar, `var_pop` made the two sides one query.
        let v = ok(&format!(
            "{T}\nSELECT count(*) FROM (SELECT var_pop(\"a\") AS \"v\" FROM \"t\") AS \"q\";\nSELECT count(*) FROM \"t\";"
        ));
        assert!(group_call(&v, "VAR_POP").is_some(), "var_pop folds in a group: {v}");
    }

    #[test]
    fn an_unlisted_aggregate_under_in_is_an_aggregate() {
        let v = ok(&same(r#"SELECT "k" FROM "u" WHERE "k" IN (SELECT CAST(bit_or("a") AS INTEGER) FROM "t")"#));
        assert!(group_call(&v, "BIT_OR").is_some(), "bit_or folds in a group: {v}");
    }

    #[test]
    fn opaque_aggregates_carry_postgres_return_types() {
        // Postgres's own type where it has exactly one; opaque where it follows the argument.
        for (f, ty) in [
            (r#"bool_or("a" > 1)"#, "BOOLEAN"),
            (r#"regr_count("c", "c")"#, "INTEGER"),
            (r#"corr("c", "c")"#, "REAL"),
            (r#"regr_slope("c", "c")"#, "REAL"),
            (r#"var_pop("a")"#, "VARBINARY"),
            (r#"stddev("c")"#, "VARBINARY"),
            (r#"bit_xor("a")"#, "VARBINARY"),
        ] {
            let v = ok(&same(&format!(r#"SELECT {f} FROM "t""#)));
            let op = f.split('(').next().unwrap().to_uppercase();
            let call = group_call(&v, &op).unwrap_or_else(|| panic!("{f} is not a group call: {v}"));
            assert_eq!(call["type"], ty, "{f}");
            assert_eq!(call["ignoreNulls"], false, "{f}: null handling is not asserted");
        }
    }

    #[test]
    fn an_aggregate_the_bag_does_not_determine_is_refused() {
        for f in [
            r#"any_value("a")"#,
            r#"mode() WITHIN GROUP (ORDER BY "a")"#,
            r#"percentile_cont(0.5) WITHIN GROUP (ORDER BY "a")"#,
            r#"percentile_disc(0.5) WITHIN GROUP (ORDER BY "a")"#,
            r#"rank(1) WITHIN GROUP (ORDER BY "a")"#,
            r#"cume_dist(1) WITHIN GROUP (ORDER BY "a")"#,
        ] {
            refused(&same(&format!(r#"SELECT {f} FROM "t""#)), "unmodelled aggregate");
        }
    }

    #[test]
    fn the_newer_order_sensitive_aggregates_are_refused() {
        for f in [
            r#"json_agg_strict("a")"#,
            r#"jsonb_object_agg_unique("b", "a")"#,
            r#"json_arrayagg("a")"#,
        ] {
            refused(&same(&format!(r#"SELECT {f} FROM "t""#)), "order-sensitive aggregate");
        }
    }

    #[test]
    fn a_qualified_newly_listed_aggregate_is_refused() {
        refused(&same(r#"SELECT "s".var_pop("a") FROM "t""#), "qualified builtin aggregate");
    }

    /// An aggregate whose arguments read only an enclosing query's columns is that query's
    /// (Postgres manual 4.2.7): here it folds over `t`, making the outer query return one row.
    #[test]
    fn an_aggregate_over_outer_columns_only_is_refused() {
        refused(
            &same(r#"SELECT (SELECT count("t"."a") FROM "u" WHERE "u"."k" = 1) AS "c" FROM "t""#),
            "enclosing query",
        );
        // The `FILTER` counts as part of the arguments.
        refused(
            &same(r#"SELECT (SELECT count(*) FILTER (WHERE "t"."a" > 0) FROM "u") AS "c" FROM "t""#),
            "enclosing query",
        );
        // So does a column read through a subquery inside the argument.
        refused(
            &same(r#"SELECT (SELECT max((SELECT "t"."a")) FROM "u") AS "c" FROM "t""#),
            "enclosing query",
        );
    }

    #[test]
    fn an_aggregate_reading_its_own_query_stays_its_own() {
        for q in [
            // No column at all: the subquery's.
            r#"SELECT (SELECT count(*) FROM "u" WHERE "u"."k" = "t"."a") AS "c" FROM "t""#,
            // One column of its own is enough, in the argument or in the FILTER.
            r#"SELECT (SELECT count("u"."k" + "t"."a") FROM "u") AS "c" FROM "t""#,
            r#"SELECT (SELECT count("t"."a") FILTER (WHERE "u"."k" > 0) FROM "u") AS "c" FROM "t""#,
        ] {
            ok(&same(q));
        }
    }
}

mod call_parts {
    use super::*;

    #[test]
    fn a_named_argument_is_refused() {
        // Both used to lower to the nullary `MAKE_INTERVAL()` / `JSON_OBJECT()`.
        refused(&same(r#"SELECT make_interval(days => "a") FROM "t""#), "days");
        refused(&same(r#"SELECT json_object('k' VALUE "a") FROM "t""#), "VALUE");
    }

    #[test]
    fn a_star_argument_is_refused_off_count() {
        refused(&same(r#"SELECT row_to_json("t".*) FROM "t""#), "argument");
        refused(&same(r#"SELECT coalesce(*) FROM "t""#), "`*` argument");
    }

    #[test]
    fn within_group_is_refused_even_on_a_declared_aggregate() {
        let sql = format!(
            "{T}\ndeclare aggregate function my_pct(REAL) returns REAL;\n\
             SELECT my_pct(0.5) WITHIN GROUP (ORDER BY \"a\") FROM \"t\";\n\
             SELECT my_pct(0.5) WITHIN GROUP (ORDER BY \"a\") FROM \"t\";"
        );
        refused(&sql, "WITHIN GROUP");
    }

    #[test]
    fn sql_json_clauses_are_refused() {
        refused(&same(r#"SELECT json_array("a" ABSENT ON NULL) FROM "t""#), "ABSENT ON NULL");
        refused(&same(r#"SELECT json_array("a" RETURNING jsonb) FROM "t""#), "RETURNING");
    }

    #[test]
    fn a_where_inside_an_aggregate_call_is_refused() {
        // sqlparser reads `count(a WHERE p)` in every dialect, and it used to lower as `count(a)`.
        refused(&same(r#"SELECT count("a" WHERE "a" > 1) FROM "t""#), "WHERE");
    }

    #[test]
    fn distinct_or_filter_on_a_scalar_call_is_refused() {
        // Postgres accepts either only on an aggregate. `DISTINCT` was dropped on the scalar path,
        // and `FILTER` on a call over an aggregate result.
        refused(&same(r#"SELECT upper(DISTINCT "b") FROM "t""#), "DISTINCT or FILTER");
        refused(&same(r#"SELECT coalesce(sum("a"), 0) FILTER (WHERE "a" > 1) FROM "t""#), "DISTINCT or FILTER");
        refused(&same(r#"SELECT upper(DISTINCT max("b")) FROM "t""#), "DISTINCT or FILTER");
    }

    #[test]
    fn an_ordinary_call_still_lowers() {
        let v = ok(&same(r#"SELECT upper("b"), count(*), sum(DISTINCT "a") FROM "t" GROUP BY "b""#));
        assert!(has_op(&v, "UPPER"));
        assert_eq!(group_call(&v, "SUM").expect("sum folds")["distinct"], true);
    }
}

mod select_into {
    use super::*;

    #[test]
    fn select_into_is_refused() {
        // `SELECT ... INTO x` creates `x` and returns no rows; it was lowered as the plain `SELECT`.
        refused(&same(r#"SELECT "a" INTO "x" FROM "t""#), "INTO");
        refused(&same(r#"SELECT 1 INTO "x""#), "INTO");
    }
}
