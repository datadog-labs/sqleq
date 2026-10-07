// Unless explicitly stated otherwise all files in this repository are licensed under the
// Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/).
// Copyright 2026-Present Datadog, Inc.

//! Name resolution for `ORDER BY` and `DISTINCT ON` keys, for output column names, and for the
//! items of a comma-separated `FROM`.
//!
//! Each "not identical" or "refused" test is a pair that is **not** equivalent in Postgres and used
//! to lower to byte-identical IR, or to IR that reads a name from the wrong place. Each has a
//! control beside it that keeps an equivalent spelling lowering alike. They do not run a prover;
//! they pin the lowering.

use serde_json::Value;
use sqleq_frontend::{lower_with, CatalogSource, FrontendError};

/// `t` and `u` join on `k`; `w` is a third table for the `IN` shape; `n` has columns named like a
/// function and like a type, so an output column can share a name with an input column.
const DDL: &str = r#"
create table "t" ("k" INTEGER, "a" INTEGER, "b" INTEGER, "x" VARCHAR);
create table "u" ("k" INTEGER, "a" INTEGER);
create table "w" ("id" INTEGER, "x" INTEGER);
create table "n" ("id" INTEGER, "s" VARCHAR, "lower" VARCHAR);
"#;

fn lower_in(q0: &str, q1: &str, src: CatalogSource) -> Result<Value, FrontendError> {
    lower_with(&format!("{DDL}\n{q0};\n{q1};"), src)
}

/// Whether the pair lowers to byte-identical queries, in both the declared catalog and the corpus
/// runs' inferred-seeded one (which runs the cast rules and the parameter substitution too). A pair
/// that lowers in one and not the other is a test bug, so it panics.
fn identical(q0: &str, q1: &str) -> bool {
    let mut seen = Vec::new();
    for src in [CatalogSource::Declared, CatalogSource::InferredSeeded] {
        let v = lower_in(q0, q1, src).unwrap_or_else(|e| panic!("expected Ok, got {e}"));
        seen.push(v["queries"][0] == v["queries"][1]);
    }
    assert_eq!(seen[0], seen[1], "the two catalog modes disagree");
    seen[0]
}

/// Assert the pair is refused in both catalog modes, as unsupported or as a schema error as `kind`
/// says, with `needle` in the reason.
fn refused(q0: &str, q1: &str, kind: &str, needle: &str) {
    for src in [CatalogSource::Declared, CatalogSource::InferredSeeded] {
        match (kind, lower_in(q0, q1, src)) {
            ("unsupported", Err(FrontendError::Unsupported(m))) | ("schema", Err(FrontendError::Schema(m))) => {
                assert!(m.contains(needle), "refused for {m:?}, not {needle:?}")
            }
            (_, other) => panic!("expected a {kind} refusal mentioning {needle:?}, got {other:?}"),
        }
    }
}

/// The first node anywhere in `v` that carries the field `key`.
fn find_with_field<'a>(v: &'a Value, key: &str) -> Option<&'a Value> {
    if v.get(key).is_some() {
        return Some(v);
    }
    match v {
        Value::Object(m) => m.values().find_map(|x| find_with_field(x, key)),
        Value::Array(a) => a.iter().find_map(|x| find_with_field(x, key)),
        _ => None,
    }
}

// --- ORDER BY: a qualified key is an input column ---------------------------------------------

/// `t = {(1, 1), (2, 2)}`, `u = {(1, 5), (2, 3)}`: the first returns 5 (the `u.a` of the least
/// `t.a`), the second 3. `t.a` used to be matched against the output column `a`, which is `u.a`.
#[test]
fn a_qualified_order_key_is_not_an_output_name() {
    let (a, b) = (
        r#"SELECT "u"."a" FROM "t" JOIN "u" ON "t"."k" = "u"."k" ORDER BY "t"."a" LIMIT 1"#,
        r#"SELECT "u"."a" FROM "t" JOIN "u" ON "t"."k" = "u"."k" ORDER BY "u"."a" LIMIT 1"#,
    );
    assert!(!identical(a, b));
    // Nested in a derived table, and under `IN`, where the bug was the same.
    let derived = |q: &str| format!(r#"SELECT "s"."a" FROM ({q}) AS "s""#);
    assert!(!identical(&derived(a), &derived(b)));
    let within = |q: &str| format!(r#"SELECT "w"."id" FROM "w" WHERE "w"."x" IN ({q})"#);
    assert!(!identical(&within(a), &within(b)));
}

/// `t(a, b) = {(1, 2), (2, 1)}`: `ORDER BY t.a` sorts by the input column and returns 2; the alias
/// `a` names `b`, and sorting by `b` returns 1.
#[test]
fn a_qualified_order_key_is_not_an_alias() {
    assert!(!identical(
        r#"SELECT "b" AS "a" FROM "t" ORDER BY "t"."a" LIMIT 1"#,
        r#"SELECT "b" AS "a" FROM "t" ORDER BY "b" LIMIT 1"#,
    ));
    // `t.a` is not projected, so the projection under the `Sort` is widened with it and the
    // collation points past the one output column; it used to point at the output column `a`.
    let v = lower_in(
        r#"SELECT "b" AS "a" FROM "t" ORDER BY "t"."a" LIMIT 1"#,
        r#"SELECT "b" FROM "t""#,
        CatalogSource::Declared,
    )
    .unwrap();
    let sort = find_with_field(&v["queries"][0], "collation").unwrap();
    assert_eq!(sort["collation"][0][0], 1);
    // The output name and the input column `b` are one value, so the two orderings lower alike.
    assert!(identical(
        r#"SELECT "b" AS "a" FROM "t" ORDER BY "a" LIMIT 1"#,
        r#"SELECT "b" AS "a" FROM "t" ORDER BY "b" LIMIT 1"#,
    ));
}

/// The same key is the output column when the output column is that input column, and then every
/// spelling of it lowers to the one `Sort` over the projection.
#[test]
fn a_qualified_order_key_over_its_own_column_is_that_column() {
    let spellings = [
        r#"SELECT "t"."a" FROM "t" ORDER BY "t"."a" LIMIT 1"#,
        r#"SELECT "t"."a" FROM "t" ORDER BY "a" LIMIT 1"#,
        r#"SELECT "t"."a" FROM "t" ORDER BY 1 LIMIT 1"#,
        r#"SELECT "t"."a" FROM "t" ORDER BY ("a") LIMIT 1"#,
        r#"SELECT "t"."a" FROM "t" ORDER BY (1) LIMIT 1"#,
    ];
    for q in &spellings[1..] {
        assert!(identical(spellings[0], q), "{q}");
    }
}

/// In a grouped query nothing above the `Group` can address the FROM scope, so a qualified key is
/// only usable when the select list has the same expression. `t.a` is not projected here, so the
/// pair is refused rather than sorted by the output column that happens to be called `a`.
#[test]
fn a_qualified_order_key_under_group_by_is_refused_unless_projected() {
    refused(
        r#"SELECT "u"."a" FROM "t" JOIN "u" ON "t"."k" = "u"."k" GROUP BY "t"."a", "u"."a" ORDER BY "t"."a" LIMIT 1"#,
        r#"SELECT "u"."a" FROM "t" JOIN "u" ON "t"."k" = "u"."k" GROUP BY "t"."a", "u"."a" ORDER BY "u"."a" LIMIT 1"#,
        "unsupported",
        "ORDER BY key is not an output column",
    );
    // Written as a select-list item, it is that item, in a grouped query and a DISTINCT one alike.
    assert!(identical(
        r#"SELECT "t"."a", COUNT(*) FROM "t" GROUP BY "t"."a" ORDER BY "t"."a" LIMIT 1"#,
        r#"SELECT "t"."a", COUNT(*) FROM "t" GROUP BY "t"."a" ORDER BY 1 LIMIT 1"#,
    ));
    assert!(identical(
        r#"SELECT DISTINCT "t"."a" FROM "t" ORDER BY "t"."a" LIMIT 1"#,
        r#"SELECT DISTINCT "t"."a" FROM "t" ORDER BY 1 LIMIT 1"#,
    ));
}

/// Above a `DISTINCT`, an input expression is usable when it is the value of an output column,
/// however that column was written — Postgres's rule, which requires the expression in the select
/// list — and refused when it is not, since nothing above the `Group` can address it.
#[test]
fn a_qualified_order_key_under_distinct_must_be_an_output_value() {
    // `t.*` outputs `k, a, b, x`, so `t.a` is its second column.
    assert!(identical(
        r#"SELECT DISTINCT "t".* FROM "t" JOIN "u" ON "t"."k" = "u"."k" ORDER BY "t"."a" DESC LIMIT 1"#,
        r#"SELECT DISTINCT "t".* FROM "t" JOIN "u" ON "t"."k" = "u"."k" ORDER BY 2 DESC LIMIT 1"#,
    ));
    assert!(identical(
        r#"SELECT DISTINCT "a" FROM "t" ORDER BY "t"."a" LIMIT 1"#,
        r#"SELECT DISTINCT "a" FROM "t" ORDER BY 1 LIMIT 1"#,
    ));
    refused(
        r#"SELECT DISTINCT "t".* FROM "t" JOIN "u" ON "t"."k" = "u"."k" ORDER BY "u"."a" LIMIT 1"#,
        r#"SELECT DISTINCT "t".* FROM "t" JOIN "u" ON "t"."k" = "u"."k" ORDER BY 2 LIMIT 1"#,
        "unsupported",
        "ORDER BY key is not an output column",
    );
}

// --- DISTINCT ON keys resolve as ORDER BY keys do -----------------------------------------------

/// `t = {(1, 1), (2, 1)}`: in the first the key `a` is the output column `a`, which is `t.b`, so it
/// returns one row; the second keys on `t.a` and returns two.
#[test]
fn a_distinct_on_key_is_an_output_name_first() {
    assert!(!identical(
        r#"SELECT DISTINCT ON ("a") "a" AS "b", "b" AS "a" FROM "t""#,
        r#"SELECT DISTINCT ON ("t"."a") "a" AS "b", "b" AS "a" FROM "t""#,
    ));
    assert!(identical(
        r#"SELECT DISTINCT ON ("a") "a" AS "b", "b" AS "a" FROM "t""#,
        r#"SELECT DISTINCT ON ("t"."b") "a" AS "b", "b" AS "a" FROM "t""#,
    ));
}

/// `t = {(1, 0), (2, 0)}`: `DISTINCT ON (1)` keys on the first output column and returns both rows;
/// `1 + 0` is a constant, one group, one row. `1` used to be lowered as the constant too.
#[test]
fn a_distinct_on_integer_is_a_position() {
    assert!(!identical(
        r#"SELECT DISTINCT ON (1) "a" FROM "t""#,
        r#"SELECT DISTINCT ON (1 + 0) "a" FROM "t""#,
    ));
    assert!(identical(r#"SELECT DISTINCT ON (1) "a" FROM "t""#, r#"SELECT DISTINCT ON ("a") "a" FROM "t""#));
}

// --- Output names ------------------------------------------------------------------------------

/// `t(a, b) = {(1, 2), (2, 1)}`: the unquoted key `A` is `a`. The output column `"A"` is another
/// name, so the first sorts by the input column `t.a` and returns 2; the second's output column is
/// `a` and it sorts by `b`, returning 1. Output names used to be lower-cased whatever the quoting.
#[test]
fn a_quoted_alias_keeps_its_case() {
    assert!(!identical(
        r#"SELECT "b" AS "A" FROM "t" ORDER BY A LIMIT 1"#,
        r#"SELECT "b" AS a FROM "t" ORDER BY A LIMIT 1"#,
    ));
    // The quoted key names the quoted alias, as the folded key names the unquoted one.
    assert!(identical(
        r#"SELECT "b" AS "A" FROM "t" ORDER BY "A" LIMIT 1"#,
        r#"SELECT "b" AS a FROM "t" ORDER BY A LIMIT 1"#,
    ));
}

/// Postgres names an unaliased cast of a column after the column, so `ORDER BY x` sorts by the
/// cast while `ORDER BY t.x` sorts by the text: `t.x = {'10', '9'}` returns 9 and 10. The cast's
/// output column used to have no name at all, so `x` reached the input column.
#[test]
fn an_unaliased_cast_is_named_after_its_column() {
    assert!(!identical(
        r#"SELECT "x"::int FROM "t" ORDER BY "x" LIMIT 1"#,
        r#"SELECT "x"::int FROM "t" ORDER BY "t"."x" LIMIT 1"#,
    ));
    // Unaliased, `CAST(a AS TEXT)` is the output column `a`, so the key `a` sorts by the text, not
    // by the input column `a` that `z` projects. (Spelled differently on the two sides, so that
    // `normalize::strip_identical_pagination` leaves the clauses for lowering to read.)
    assert!(identical(
        r#"SELECT CAST("a" AS TEXT), "a" AS "z" FROM "t" ORDER BY "a" LIMIT 1"#,
        r#"SELECT CAST("a" AS TEXT), "a" AS "z" FROM "t" ORDER BY 1 LIMIT 1"#,
    ));
    assert!(!identical(
        r#"SELECT CAST("a" AS TEXT), "a" AS "z" FROM "t" ORDER BY "a" LIMIT 1"#,
        r#"SELECT CAST("a" AS TEXT), "a" AS "z" FROM "t" ORDER BY "z" LIMIT 1"#,
    ));
    // `count(*)` is the output column `count`.
    assert!(identical(
        r#"SELECT COUNT(*) FROM "t" GROUP BY "b" ORDER BY "count" LIMIT 1"#,
        r#"SELECT COUNT(*) FROM "t" GROUP BY "b" ORDER BY 1 LIMIT 1"#,
    ));
}

/// A call is named after its function: `ORDER BY lower` is the output column, not `n.lower`.
#[test]
fn an_unaliased_call_is_named_after_its_function() {
    assert!(!identical(
        r#"SELECT lower("s") FROM "n" ORDER BY "lower" LIMIT 1"#,
        r#"SELECT lower("s") FROM "n" ORDER BY "n"."lower" LIMIT 1"#,
    ));
    assert!(identical(
        r#"SELECT lower("s") FROM "n" ORDER BY "lower" LIMIT 1"#,
        r#"SELECT lower("s") FROM "n" ORDER BY 1 LIMIT 1"#,
    ));
    // An operator's column is `?column?`, which no ordinary key names.
    assert!(identical(
        r#"SELECT "id" + 1 FROM "n" ORDER BY "lower" LIMIT 1"#,
        r#"SELECT "id" + 1 FROM "n" ORDER BY "n"."lower" LIMIT 1"#,
    ));
}

/// A `CASE` is named after its `ELSE` or after `case`, and a cast of anything but a column or call
/// after its type, which the cast rules may have rewritten. Where the name is not known, a bare key
/// that names no known output column could still be that column, so it is refused.
#[test]
fn a_bare_key_beside_an_output_column_of_unknown_name_is_refused() {
    refused(
        r#"SELECT CASE WHEN "id" > 0 THEN "s" END FROM "n" ORDER BY "lower" LIMIT 1"#,
        r#"SELECT CASE WHEN "id" > 0 THEN "s" END FROM "n" ORDER BY "id" LIMIT 1"#,
        "unsupported",
        "whose name is not known",
    );
    // A qualified key is never an output name, so it is unaffected.
    assert!(!identical(
        r#"SELECT CASE WHEN "id" > 0 THEN "s" END FROM "n" ORDER BY "n"."lower" LIMIT 1"#,
        r#"SELECT CASE WHEN "id" > 0 THEN "s" END FROM "n" ORDER BY "n"."id" LIMIT 1"#,
    ));
}

// --- Comma-separated FROM items ----------------------------------------------------------------

const ABC: &str = r#"
create table "a" ("id" INTEGER, "x" INTEGER);
create table "b" ("id" INTEGER, "x" INTEGER);
create table "c" ("id" INTEGER, "x" INTEGER, "z" INTEGER);
create table "o" ("id" INTEGER, "x" INTEGER);
"#;

fn abc(q0: &str, q1: &str) -> Result<Value, FrontendError> {
    lower_with(&format!("{ABC}\n{q0};\n{q1};"), CatalogSource::Declared)
}

fn abc_identical(q0: &str, q1: &str) -> bool {
    let v = abc(q0, q1).unwrap_or_else(|e| panic!("expected Ok, got {e}"));
    v["queries"][0] == v["queries"][1]
}

/// `a = {}`, `b = {(2, 1)}`, `c = {(3, 1, 0)}`: the comma binds loosest, so the first crosses the
/// empty `a` with `b RIGHT JOIN c` and returns nothing, where `(a CROSS JOIN b) RIGHT JOIN c` keeps
/// `c`'s row. The same for `FULL`.
#[test]
fn an_outer_join_after_a_comma_item_is_not_grouped_with_it() {
    for kind in ["RIGHT", "FULL"] {
        assert!(
            !abc_identical(
                &format!(r#"SELECT "c"."id" FROM "a", "b" {kind} JOIN "c" ON "b"."x" = "c"."x""#),
                &format!(r#"SELECT "c"."id" FROM "a" CROSS JOIN "b" {kind} JOIN "c" ON "b"."x" = "c"."x""#),
            ),
            "{kind}"
        );
        let v = abc(
            &format!(r#"SELECT "c"."id" FROM "a", "b" {kind} JOIN "c" ON "b"."x" = "c"."x""#),
            r#"SELECT 1 FROM "a""#,
        )
        .unwrap();
        // A cross join of `a` with the outer join, whose condition numbers `b` from 0.
        let top = &find_with_field(&v["queries"][0], "join").unwrap()["join"];
        assert_eq!(top["kind"], "INNER");
        assert_eq!(top["right"]["join"]["kind"], kind);
        let cond = &top["right"]["join"]["condition"]["operand"];
        assert_eq!((cond[0]["column"].as_u64(), cond[1]["column"].as_u64()), (Some(1), Some(3)));
    }
}

/// `a = {(1, 1)}`, `b = {(2, 2)}`, `c = {(3, 1, 0)}`: in the first `USING (x)` joins `b` and `c`
/// and nothing matches; in the second it joins `a` and `c`. The name used to be found on `a` first.
#[test]
fn using_after_a_comma_item_looks_only_at_its_own_join() {
    assert!(!abc_identical(
        r#"SELECT "a"."id", "b"."id", "c"."id" FROM "a", "b" JOIN "c" USING ("x")"#,
        r#"SELECT "a"."id", "b"."id", "c"."id" FROM "a" JOIN "c" USING ("x"), "b""#,
    ));
    // `b.x = c.x`, numbered inside `b JOIN c`; it used to be `a.x = c.x`.
    let v = abc(r#"SELECT 1 FROM "a", "b" JOIN "c" USING ("x")"#, r#"SELECT 1 FROM "a""#).unwrap();
    let top = &find_with_field(&v["queries"][0], "join").unwrap()["join"];
    let cond = &top["right"]["join"]["condition"]["operand"];
    assert_eq!((cond[0]["column"].as_u64(), cond[1]["column"].as_u64()), (Some(1), Some(3)));
}

/// `a` is not visible in the `ON` of `b JOIN c`, so the bare `x` there is the enclosing query's
/// `o.x`, not `a.x`. With `o = {(1, 7)}`, `a = {(10, 5)}`, `b = {(20, 0)}`, `c = {(30, 0, 5)}` the
/// first tests `5 = 7` and the second `5 = 5`.
#[test]
fn an_on_condition_after_a_comma_item_does_not_see_it() {
    assert!(!abc_identical(
        r#"SELECT "o"."id" FROM "o" WHERE EXISTS (SELECT 1 FROM "a", "b" JOIN "c" ON "c"."z" = "x")"#,
        r#"SELECT "o"."id" FROM "o" WHERE EXISTS (SELECT 1 FROM "a", "b" CROSS JOIN "c" WHERE "c"."z" = "a"."x")"#,
    ));
    // Without the enclosing query there is nothing to resolve it to; Postgres rejects it too.
    match abc(r#"SELECT 1 FROM "a", "b" JOIN "c" ON "c"."z" = "a"."x""#, r#"SELECT 1 FROM "a""#) {
        Err(FrontendError::Schema(m)) => assert!(m.contains("a.x"), "{m}"),
        other => panic!("expected an unresolved-column refusal, got {other:?}"),
    }
}

/// The comma items' own joins keep the shape they had: each item is a join input, and inner joins
/// and plain products lower as before.
#[test]
fn comma_items_without_outer_joins_keep_their_shape() {
    assert!(abc_identical(
        r#"SELECT "a"."id" FROM "a", "b", "c""#,
        r#"SELECT "a"."id" FROM "a" CROSS JOIN "b" CROSS JOIN "c""#,
    ));
    assert!(abc_identical(
        r#"SELECT "a"."id" FROM "a" JOIN "b" ON "a"."x" = "b"."x", "c""#,
        r#"SELECT "a"."id" FROM "a" JOIN "b" ON "a"."x" = "b"."x" CROSS JOIN "c""#,
    ));
}

/// Postgres raises "common column name x appears more than once in left table" when the left side
/// of a `USING` has two `x` columns: there is no one column to compare. It used to take the first.
#[test]
fn a_using_name_twice_on_one_side_is_refused() {
    match abc(r#"SELECT 1 FROM "a" JOIN "b" ON TRUE JOIN "c" USING ("x")"#, r#"SELECT 1 FROM "a""#) {
        Err(FrontendError::Schema(m)) => assert!(m.contains("appears more than once on the left"), "{m}"),
        other => panic!("expected the common-column refusal, got {other:?}"),
    }
    match abc(r#"SELECT 1 FROM "c" JOIN ("a" JOIN "b" ON TRUE) USING ("x")"#, r#"SELECT 1 FROM "a""#) {
        Err(FrontendError::Schema(m)) => assert!(m.contains("appears more than once on the right"), "{m}"),
        other => panic!("expected the common-column refusal, got {other:?}"),
    }
    // A name an earlier `USING` merged is one column, so a chain of them still lowers.
    abc(r#"SELECT 1 FROM "a" JOIN "b" USING ("x") JOIN "c" USING ("x")"#, r#"SELECT 1 FROM "a""#).unwrap();
    abc(r#"SELECT 1 FROM "c" JOIN ("a" JOIN "b" USING ("x")) USING ("x")"#, r#"SELECT 1 FROM "a""#).unwrap();
}
