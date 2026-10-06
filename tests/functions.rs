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
