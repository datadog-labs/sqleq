// Unless explicitly stated otherwise all files in this repository are licensed under the
// Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/).
// Copyright 2026-Present Datadog, Inc.

//! A subquery's `ORDER BY` is observable to what reads its rows (issues #59 and #123).
//!
//! Postgres hands an enclosing query a subquery's rows in the order its `ORDER BY` sorted them, and
//! an operation above that keeps the first or the last of them, or folds them in order, sees it:
//! `string_agg` over `(SELECT x FROM t ORDER BY x)` concatenates in that order; `DISTINCT`,
//! `GROUP BY`, `UNION`, `min` and `max` keep one member of a class of values `=` calls equal
//! (`2.0` and `2.00`), which a cast to text then tells apart; and `LIMIT` or `DISTINCT ON` without
//! an ordering of their own keep the rows that order puts first. Every non-equivalent pair here
//! sorts one subquery `ASC` on one side and `DESC` on the other, and every one was settled as one
//! query: `reflexive` (the reflexivity check stripped both orderings) or `emit-reflexive` (the
//! lowering dropped them, and the checks that refuse a read telling two members apart let it through
//! because the two plans were equal). The controls keep their answers: over integers the member kept
//! is the only one there is, the outermost query's `ORDER BY` has nothing above it, and a slice whose
//! ordering keys every column it returns is not decided by the order of its input.

use serde_json::Value;
use sqleq_frontend::{lower_sql, reflexive, FrontendError};

fn pair(ddl: &str, q0: &str, q1: &str) -> String {
    format!("{ddl}\n{q0};\n{q1};")
}

/// `q` with its innermost `ORDER BY k` (written `{dir}`) sorted ascending and then descending.
fn sorted_both_ways(q: &str) -> (String, String) {
    (q.replace("{dir}", "ASC"), q.replace("{dir}", "DESC"))
}

/// The two lowered queries, or the refusal.
fn lowered(src: &str) -> Result<(Value, Value), FrontendError> {
    lower_sql(src).map(|v| (v["queries"][0].clone(), v["queries"][1].clone()))
}

/// The pair is not settled as one query: lowering refuses it, and its two sides do not normalize to
/// one tree either.
fn not_one_query(ddl: &str, q: &str) {
    let (q0, q1) = sorted_both_ways(q);
    let src = pair(ddl, &q0, &q1);
    match lowered(&src) {
        Err(FrontendError::Unsupported(_)) => {}
        Err(e) => panic!("expected an unsupported refusal, got {e}\n{q0}\n{q1}"),
        Ok((a, b)) if a == b => panic!("the two sides lowered to one plan\n{q0}\n{q1}"),
        Ok(_) => panic!("expected a refusal, but it lowered\n{q0}\n{q1}"),
    }
    assert!(!reflexive(&src), "the two sides normalize to one query\n{q0}\n{q1}");
}

/// The pair still lowers to one plan.
fn still_one_plan(ddl: &str, q0: &str, q1: &str) {
    match lowered(&pair(ddl, q0, q1)) {
        Ok((a, b)) => assert_eq!(a, b, "expected one plan\n{q0}\n{q1}"),
        Err(e) => panic!("expected one plan, got {e}\n{q0}\n{q1}"),
    }
}

const T: &str = r#"create table "t" ("id" INTEGER, "x" TEXT, "g" INTEGER);"#;
const NUMERIC: &str = r#"create table "u" ("k" INTEGER, "n" NUMERIC);"#;

/// Issue #59: `t = {(1, 'b'), (2, 'a'), (3, 'c')}` gives `a,b,c` and `c,b,a`, `{a,b,c}` and
/// `{c,b,a}`, `["a", "b", "c"]` and `["c", "b", "a"]` on Postgres 17. Lowering refuses these
/// aggregates; the reflexivity check stripped the subquery's `ORDER BY` and credited the pair.
#[test]
fn an_order_sensitive_aggregate_over_a_sorted_subquery_is_not_one_query() {
    for agg in [r#"string_agg("s"."x", ',')"#, r#"array_agg("s"."x")"#, r#"json_agg("s"."x")"#] {
        not_one_query(T, &format!(r#"SELECT {agg} FROM (SELECT "x" FROM "t" ORDER BY "x" {{dir}}) AS "s""#));
    }
}

/// The aggregate-level form was refused on both paths before, and still is.
#[test]
fn an_ordered_aggregate_call_is_still_refused() {
    not_one_query(T, r#"SELECT string_agg("x", ',' ORDER BY "x" {dir}) FROM "t""#);
}

/// Issue #123, the main pair and its neighbours: over `u = {(1, 2.0), (2, 2.00)}` each pair returns
/// `'2.0'` on one side and `'2.00'` on the other (Postgres 17, `max_parallel_workers_per_gather =
/// 0`). Each lowered to one plan, which lifted the refusal of the cast to text.
#[test]
fn a_kept_member_read_as_text_over_a_sorted_subquery_is_refused() {
    for q in [
        "SELECT CAST(x.n AS TEXT) FROM (SELECT DISTINCT n FROM (SELECT n FROM u ORDER BY k {dir}) s) x",
        "SELECT CAST(n AS TEXT) FROM (SELECT n FROM u ORDER BY k {dir}) s GROUP BY n",
        "SELECT CAST(x.n AS TEXT) FROM (SELECT n FROM (SELECT n FROM u ORDER BY k {dir}) s UNION SELECT n FROM u WHERE false) x",
        "SELECT CAST(max(n) AS TEXT) FROM (SELECT n FROM u ORDER BY k {dir}) s",
        "SELECT CAST(min(n) AS TEXT) FROM (SELECT n FROM u ORDER BY k {dir}) s",
        "SELECT DISTINCT ON (n) CAST(n AS TEXT) AS c FROM (SELECT n FROM u ORDER BY k {dir}) s",
        "SELECT x.n || '' FROM (SELECT DISTINCT n FROM (SELECT n FROM u ORDER BY k {dir}) s) x",
        "SELECT scale(x.n) FROM (SELECT DISTINCT n FROM (SELECT n FROM u ORDER BY k {dir}) s) x",
    ] {
        not_one_query(NUMERIC, q);
    }
}

/// The same with the other types whose `=` has classes of more than one member, through each of
/// the three checks that lifted their refusal on one plan.
#[test]
fn the_same_holds_for_every_type_whose_equality_is_not_identity() {
    let distinct = "SELECT CAST(x.n AS TEXT) FROM (SELECT DISTINCT n FROM (SELECT n FROM u ORDER BY k {dir}) s) x";
    for ty in ["DOUBLE PRECISION", "INTERVAL", "CITEXT", "NUMRANGE"] {
        not_one_query(&format!(r#"create table "u" ("k" INTEGER, "n" {ty});"#), distinct);
    }
    // `range_agg` keeps one member too: over `'[1.0,2.0)'` and then `'[1.00,2.00)'` it returns
    // `{[1.00,2.00)}`, and over the two the other way round `{[1.0,2.0)}`.
    not_one_query(
        r#"create table "u" ("k" INTEGER, "n" NUMRANGE);"#,
        "SELECT CAST(range_agg(n) AS TEXT) FROM (SELECT n FROM u ORDER BY k {dir}) s",
    );
    not_one_query(
        r#"create table "u" ("k" INTEGER, "n" JSONB);"#,
        "SELECT x.n ->> 'a' FROM (SELECT DISTINCT n FROM (SELECT n FROM u ORDER BY k {dir}) s) x",
    );
    not_one_query(
        r#"create collation "ci" (provider = icu, locale = 'und-u-ks-level2', deterministic = false);
create table "u" ("k" INTEGER, "n" TEXT COLLATE "ci");"#,
        "SELECT ascii(x.n) FROM (SELECT DISTINCT n FROM (SELECT n FROM u ORDER BY k {dir}) s) x",
    );
}

/// Issue #123's reflexivity-path pair: lowering refuses `random()`, and both sides used to
/// normalize to one query.
#[test]
fn a_refused_pair_is_not_reflexive_through_a_kept_member() {
    not_one_query(
        NUMERIC,
        "SELECT CAST(x.n AS TEXT), random() > 2 FROM (SELECT DISTINCT n FROM (SELECT n FROM u ORDER BY k {dir}) s) x",
    );
}

/// Issue #123, over integers: `t = {(1, 1, 1), (2, 1, 1)}` gives `1` and `2` in both pairs. A slice
/// or `DISTINCT ON` with no ordering of its own keeps the rows the subquery's order puts first, and
/// the lowering dropped that order.
#[test]
fn a_slice_or_distinct_on_over_a_sorted_subquery_is_refused() {
    not_one_query(T, "SELECT id FROM (SELECT id FROM t ORDER BY id {dir}) s LIMIT 1");
    not_one_query(T, "SELECT id FROM (SELECT id FROM t ORDER BY id {dir}) s OFFSET 1");
    not_one_query(T, "SELECT id FROM (SELECT id FROM t ORDER BY id {dir}) s FETCH FIRST 1 ROW ONLY");
    not_one_query(T, "SELECT DISTINCT ON (g) id FROM (SELECT g, id FROM t ORDER BY id {dir}) s");
    // An ordering that leaves the returned column out leaves ties to the input order.
    not_one_query(T, "SELECT id FROM (SELECT id, g FROM t ORDER BY id {dir}) s ORDER BY g LIMIT 1");
    // A slice reads the order of a subquery wherever it sits below it.
    not_one_query(T, "SELECT id FROM (SELECT id FROM (SELECT id FROM t ORDER BY id {dir}) a) b LIMIT 1");
    not_one_query(T, "(SELECT id FROM t ORDER BY id {dir}) LIMIT 1");
}

/// The reflexivity check has no types, so every construct that can see a subquery's order keeps
/// the ordering, whatever it is applied to. Each pair here would otherwise be one query to it.
#[test]
fn the_reflexivity_check_keeps_an_ordering_any_consumer_can_see() {
    let sub = r#"(SELECT "x", "id" FROM "t" ORDER BY "id" {dir}) AS "s""#;
    for select in [
        // A window numbers the rows in the order they arrive: `row_number() OVER ()` over the
        // ascending subquery pairs 1 with 'b', over the descending one with 'c'.
        r#"SELECT row_number() OVER (), "x" FROM {sub}"#,
        r#"SELECT "x", count(*) OVER (PARTITION BY "x") FROM {sub}"#,
        // `ARRAY(SELECT ...)` builds the array in the order of its query's rows.
        r#"SELECT ARRAY(SELECT "x" FROM "t" ORDER BY "x" {dir})"#,
        r#"SELECT DISTINCT "x" FROM {sub}"#,
        r#"SELECT "x" FROM {sub} GROUP BY "x""#,
        r#"SELECT "x" FROM {sub} UNION SELECT "x" FROM "t""#,
        r#"SELECT "x" FROM {sub} INTERSECT SELECT "x" FROM "t""#,
        r#"SELECT "x" FROM {sub} EXCEPT ALL SELECT "x" FROM "t""#,
        r#"SELECT max("x") FROM {sub}"#,
        r#"SELECT pg_catalog.min("x") FROM {sub}"#,
        r#"SELECT sum("id") FROM {sub}"#,
        r#"SELECT corr("id", "id") FROM {sub}"#,
        r#"SELECT any_value("x") FROM {sub}"#,
        r#"SELECT jsonb_agg("x") FROM {sub}"#,
        // In a subquery of its own, not the one that is sorted.
        r#"SELECT "x" FROM {sub} WHERE "id" = (SELECT max("id") FROM "t")"#,
    ] {
        let q = select.replace("{sub}", sub);
        let (q0, q1) = sorted_both_ways(&q);
        assert!(!reflexive(&pair(T, &q0, &q1)), "the two sides normalize to one query\n{q0}\n{q1}");
    }
}

/// Where nothing reads the order a subquery hands on, its `ORDER BY` is still dead to the
/// reflexivity check: a count, a fold over booleans, a concatenation of branches, a projection.
#[test]
fn the_reflexivity_check_still_strips_an_ordering_nothing_reads() {
    let sub = r#"(SELECT "x", "id" FROM "t" ORDER BY "id" {dir}) AS "s""#;
    for select in [
        r#"SELECT "x" FROM {sub}"#,
        r#"SELECT count(*) FROM {sub}"#,
        r#"SELECT count(DISTINCT "x") FROM {sub}"#,
        r#"SELECT bool_or("id" > 1) FROM {sub}"#,
        r#"SELECT "x" FROM {sub} UNION ALL SELECT "x" FROM "t""#,
        r#"SELECT "x" FROM "t" WHERE "id" IN (SELECT "id" FROM "t" ORDER BY "id" {dir})"#,
    ] {
        let q = select.replace("{sub}", sub);
        let (q0, q1) = sorted_both_ways(&q);
        assert!(reflexive(&pair(T, &q0, &q1)), "expected one query\n{q0}\n{q1}");
    }
}

/// The outermost query's own `ORDER BY` has nothing above it to observe it: on both paths it is
/// dead, with a consumer of a subquery's order in the query or without one.
#[test]
fn the_outermost_order_by_stays_dead() {
    for q in [
        r#"SELECT "x" FROM "t" ORDER BY "x" {dir}"#,
        r#"SELECT DISTINCT "x" FROM "t" ORDER BY "x" {dir}"#,
        r#"SELECT "g", max("id") FROM "t" GROUP BY "g" ORDER BY "g" {dir}"#,
    ] {
        let (q0, q1) = sorted_both_ways(q);
        still_one_plan(T, &q0, &q1);
        assert!(reflexive(&pair(T, &q0, &q1)), "expected one query\n{q0}\n{q1}");
    }
    // A read that tells two members apart is still let through on one plan where no subquery's
    // `ORDER BY` was dropped.
    let (q0, q1) = sorted_both_ways("SELECT CAST(x.n AS TEXT) FROM (SELECT DISTINCT n FROM u) x ORDER BY 1 {dir}");
    still_one_plan(NUMERIC, &q0, &q1);
    // And so is a read of each row's own value, where nothing keeps one of several: the ordering
    // reaches no consumer, so it is stripped before lowering.
    let (q0, q1) = sorted_both_ways("SELECT CAST(s.n AS TEXT) FROM (SELECT n FROM u ORDER BY k {dir}) s");
    still_one_plan(NUMERIC, &q0, &q1);
}

/// Over integers, the member `DISTINCT`, `GROUP BY` or `max` keeps is the only one there is, so the
/// order of their input changes nothing and the pairs still lower to one plan. So do the same pairs
/// over `numeric` with no read that tells members apart, since equal values are the same rows.
#[test]
fn a_kept_member_nothing_tells_apart_still_lowers_to_one_plan() {
    let int = r#"create table "u" ("k" INTEGER, "n" INTEGER);"#;
    for (ddl, q) in [
        (int, "SELECT CAST(x.n AS TEXT) FROM (SELECT DISTINCT n FROM (SELECT n FROM u ORDER BY k {dir}) s) x"),
        (int, "SELECT CAST(n AS TEXT) FROM (SELECT n FROM u ORDER BY k {dir}) s GROUP BY n"),
        (int, "SELECT CAST(max(n) AS TEXT) FROM (SELECT n FROM u ORDER BY k {dir}) s"),
        (int, "SELECT x.n FROM (SELECT n FROM (SELECT n FROM u ORDER BY k {dir}) s UNION SELECT n FROM u) x"),
        (NUMERIC, "SELECT x.n FROM (SELECT DISTINCT n FROM (SELECT n FROM u ORDER BY k {dir}) s) x"),
        (NUMERIC, "SELECT max(n) FROM (SELECT n FROM u ORDER BY k {dir}) s"),
    ] {
        let (q0, q1) = sorted_both_ways(q);
        still_one_plan(ddl, &q0, &q1);
    }
}

/// A slice whose own ordering keys every column it returns keeps the same bag of values whatever
/// order its input arrives in, so the subquery's `ORDER BY` below it is irrelevant.
#[test]
fn a_slice_ordered_by_every_column_it_returns_still_lowers() {
    for q in [
        "SELECT id FROM (SELECT id FROM t ORDER BY id {dir}) s ORDER BY id LIMIT 1",
        "SELECT id FROM (SELECT id FROM t ORDER BY id {dir}) s ORDER BY 1 DESC LIMIT 1",
        "SELECT id FROM (SELECT id, g FROM t ORDER BY id {dir}) s ORDER BY g, id LIMIT 1",
        "SELECT s.id FROM (SELECT id FROM t ORDER BY id {dir}) s ORDER BY s.id LIMIT 1",
    ] {
        let (q0, q1) = sorted_both_ways(q);
        still_one_plan(T, &q0, &q1);
    }
}

/// The same pair twice is one query whatever orderings the lowering drops from it, so a read the
/// one-plan exemption lets through stays let through.
#[test]
fn the_same_query_twice_is_still_one_plan() {
    let q = "SELECT CAST(x.n AS TEXT) FROM (SELECT DISTINCT n FROM (SELECT n FROM u ORDER BY k) s) x";
    still_one_plan(NUMERIC, q, q);
}
