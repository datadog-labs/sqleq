// Unless explicitly stated otherwise all files in this repository are licensed under the
// Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/).
// Copyright 2026-Present Datadog, Inc.

//! Integration tests for the public `lower_sql` API. These check parsing/lowering shape and the
//! soundness refusals; they do not run the prover.

use sqleq_frontend::{lower_sql, lower_with, lower_with_ddl, CatalogSource, FrontendError};

const T: &str = r#"create table "t" ("a" INTEGER, "b" VARCHAR, "c" VARBINARY, unique ("a"));"#;

fn ok(sql: &str) -> serde_json::Value {
    lower_sql(sql).unwrap_or_else(|e| panic!("expected Ok, got {e}"))
}

/// The first node anywhere in `v` whose `"operator"` is `op`. Several tests below are about the
/// *shape* of one operator node (its operand count, the type on a NULL), which a substring check on
/// the serialised JSON cannot express.
fn find_op<'a>(v: &'a serde_json::Value, op: &str) -> Option<&'a serde_json::Value> {
    if v.get("operator").and_then(|o| o.as_str()) == Some(op) {
        return Some(v);
    }
    match v {
        serde_json::Value::Object(m) => m.values().find_map(|x| find_op(x, op)),
        serde_json::Value::Array(a) => a.iter().find_map(|x| find_op(x, op)),
        _ => None,
    }
}

/// The first node anywhere in `v` that carries the field `key`. Used to reach a node without
/// spelling out the path to it, so a test asserts about the node it names rather than about the
/// nesting that happens to surround it today.
fn find_with_field<'a>(v: &'a serde_json::Value, key: &str) -> Option<&'a serde_json::Value> {
    if v.get(key).is_some() {
        return Some(v);
    }
    match v {
        serde_json::Value::Object(m) => m.values().find_map(|x| find_with_field(x, key)),
        serde_json::Value::Array(a) => a.iter().find_map(|x| find_with_field(x, key)),
        _ => None,
    }
}

/// Assert `sql` is refused as unsupported, with `needle` in the reason.
fn refused(sql: &str, needle: &str) {
    match lower_sql(sql) {
        Err(FrontendError::Unsupported(m)) => {
            assert!(m.contains(needle), "refused, but for {m:?} rather than {needle:?}")
        }
        Err(e) => panic!("expected an Unsupported refusal mentioning {needle:?}, got {e}"),
        Ok(_) => panic!("expected an Unsupported refusal mentioning {needle:?}, but it lowered"),
    }
}

/// Every value under a `key` field anywhere in `v` — for the tests that need *both* sides' nodes
/// rather than the first one [`find_with_field`] happens to reach.
fn collect_field(v: &serde_json::Value, key: &str) -> Vec<serde_json::Value> {
    let mut out = Vec::new();
    let mut stack = vec![v];
    while let Some(x) = stack.pop() {
        match x {
            serde_json::Value::Object(m) => {
                if let Some(hit) = m.get(key) {
                    out.push(hit.clone());
                }
                stack.extend(m.values());
            }
            serde_json::Value::Array(a) => stack.extend(a.iter()),
            _ => {}
        }
    }
    out
}

/// A pair whose two sides are `q0` and `q1`, over [`T`].
fn pair(q0: &str, q1: &str) -> String {
    format!("{T}\n{q0};\n{q1};")
}

/// A pair of two copies of the same query — for tests that only care that it lowers at all.
fn same(q: &str) -> String {
    pair(q, q)
}

#[test]
fn lowers_a_simple_pair() {
    let sql = format!("{T}\nSELECT \"a\", \"b\" FROM \"t\" WHERE \"a\" = 1;\nSELECT \"a\", \"b\" FROM \"t\" WHERE \"a\" = 1;");
    let v = ok(&sql);
    assert_eq!(v["schemas"].as_array().unwrap().len(), 1);
    assert_eq!(v["queries"].as_array().unwrap().len(), 2);
    // identical queries -> identical IR
    assert_eq!(v["queries"][0], v["queries"][1]);
    // top of each query is a Project
    assert!(v["queries"][0].get("project").is_some());
}

#[test]
fn distinct_lowers_to_group() {
    let sql = format!("{T}\nSELECT DISTINCT \"a\" FROM \"t\";\nSELECT DISTINCT \"a\" FROM \"t\";");
    let v = ok(&sql);
    assert!(v["queries"][0].get("group").is_some(), "DISTINCT should lower to a Group node");
}

#[test]
fn count_lowers_to_group() {
    let sql = format!("{T}\nSELECT COUNT(*) FROM \"t\";\nSELECT COUNT(\"a\") FROM \"t\";");
    let v = ok(&sql);
    assert!(v["queries"][0].get("group").is_some());
}

#[test]
fn comparison_inserts_cast_for_mismatched_types() {
    // c is VARBINARY (opaque); comparing to an INTEGER literal must insert a CAST to a common type.
    let sql = format!("{T}\nSELECT \"a\" FROM \"t\" WHERE \"c\" = 1;\nSELECT \"a\" FROM \"t\" WHERE \"c\" = 1;");
    let v = ok(&sql);
    let s = v.to_string();
    assert!(s.contains("CAST"), "expected a CAST for the VARBINARY vs INTEGER comparison");
}

#[test]
fn ilike_is_a_distinct_operator_from_like() {
    // Both are uninterpreted predicates, so they must not share a symbol: `LIKE` and `ILIKE` on the
    // same operands are different functions and a pair that swaps one for the other is not provable.
    let sql = format!(
        "{T}\nSELECT \"a\" FROM \"t\" WHERE \"b\" LIKE 'x%';\nSELECT \"a\" FROM \"t\" WHERE \"b\" ILIKE 'x%';"
    );
    let v = ok(&sql);
    assert!(v.to_string().contains(r#""operator":"ILIKE""#), "ILIKE should lower to an ILIKE op");
    assert_ne!(v["queries"][0], v["queries"][1], "LIKE and ILIKE must not lower to the same IR");
}

#[test]
fn negated_ilike_wraps_in_not() {
    let sql = format!(
        "{T}\nSELECT \"a\" FROM \"t\" WHERE \"b\" NOT ILIKE 'x%';\nSELECT \"a\" FROM \"t\" WHERE \"b\" NOT ILIKE 'x%';"
    );
    let s = ok(&sql).to_string();
    assert!(s.contains(r#""operator":"NOT""#) && s.contains(r#""operator":"ILIKE""#));
}

#[test]
fn refuses_like_escape() {
    // ESCAPE changes which characters are wildcards; dropping it would be an unfaithful lowering.
    let sql = format!(
        "{T}\nSELECT \"a\" FROM \"t\" WHERE \"b\" LIKE 'x!%' ESCAPE '!';\nSELECT \"a\" FROM \"t\" WHERE \"b\" LIKE 'x%';"
    );
    assert!(matches!(lower_sql(&sql), Err(FrontendError::Unsupported(m)) if m.contains("ESCAPE")));
}

#[test]
fn not_null_and_primary_key_reach_the_schema() {
    // Nullability is load-bearing: `i1.x = i2.x` is a no-op self-join only when x cannot be NULL.
    // `a` is NOT NULL outright, `b` inherits it from PRIMARY KEY, `c` stays nullable.
    let ddl = r#"create table "u" ("a" INTEGER NOT NULL, "b" INTEGER PRIMARY KEY, "c" INTEGER);"#;
    let sql = format!("{ddl}\nSELECT \"a\" FROM \"u\";\nSELECT \"a\" FROM \"u\";");
    let v = ok(&sql);
    assert_eq!(v["schemas"][0]["nullable"], serde_json::json!([false, false, true]));
    // PRIMARY KEY is also a key.
    assert_eq!(v["schemas"][0]["key"], serde_json::json!([[1]]));
}

#[test]
fn columns_default_to_nullable() {
    let v = ok(&format!("{T}\nSELECT \"a\" FROM \"t\";\nSELECT \"a\" FROM \"t\";"));
    assert_eq!(v["schemas"][0]["nullable"], serde_json::json!([true, true, true]));
}

// --- Pagination -> Sort ------------------------------------------------------------------------
//
// Every pair below gives its two sides *different* pagination on purpose. With identical clauses
// `normalize::strip_identical_pagination` removes them before lowering ever sees them, so a pair of
// two copies would test the strip rather than the emission.

/// The collation index is a 0-based position in the query's own output tuple, not a de-Bruijn level
/// like every other column reference in the IR. `SELECT "b", "a"` puts `a` second, so the key on `a`
/// is index 1 — the same convention as the prover's own `testSortProjectTranspose1.json` fixture.
#[test]
fn limit_lowers_to_a_sort() {
    let v = ok(&pair(
        r#"SELECT "b", "a" FROM "t" ORDER BY "a" LIMIT 5 OFFSET 2"#,
        r#"SELECT "b", "a" FROM "t""#,
    ));
    let s = find_with_field(&v["queries"][0], "collation").expect("a sort node");
    assert_eq!(s["collation"], serde_json::json!([[1, "INTEGER", "ASCENDING NULLS LAST"]]));
    assert_eq!(s["limit"]["operator"], "5");
    assert_eq!(s["offset"]["operator"], "2");
    // The unpaginated side gets no Sort at all.
    assert!(find_with_field(&v["queries"][1], "collation").is_none());
}

/// `ORDER BY` with nothing to slice is unobservable under bag semantics, and is still dropped.
#[test]
fn order_by_without_a_slice_is_dropped() {
    let v = ok(&same(r#"SELECT "a" FROM "t" ORDER BY "a""#));
    assert!(find_with_field(&v["queries"][0], "collation").is_none());
}

/// `OFFSET` alone drops rows, which makes the order observable, so it counts as a slice.
#[test]
fn offset_alone_is_a_slice() {
    let v = ok(&pair(r#"SELECT "a" FROM "t" ORDER BY "a" OFFSET 3"#, r#"SELECT "a" FROM "t""#));
    let s = find_with_field(&v["queries"][0], "collation").expect("a sort node");
    assert_eq!(s["limit"], serde_json::Value::Null);
    assert_eq!(s["offset"]["operator"], "3");
}

/// A slice with no ordering is legal: the collation is empty and the prover gets a bare `limit` HOp.
#[test]
fn a_bare_limit_has_an_empty_collation() {
    let v = ok(&pair(r#"SELECT "a" FROM "t" LIMIT 5"#, r#"SELECT "a" FROM "t""#));
    let s = find_with_field(&v["queries"][0], "collation").expect("a sort node");
    assert_eq!(s["collation"], serde_json::json!([]));
    assert_eq!(s["limit"]["operator"], "5");
}

/// Spelling out a default must not change the IR, or the same ordering written two ways would fail
/// to prove equal. Ascending defaults to `NULLS LAST` and descending to `NULLS FIRST`.
#[test]
fn the_order_defaults_are_resolved_not_passed_through() {
    let v = ok(&pair(
        r#"SELECT "a" FROM "t" ORDER BY "a" LIMIT 5"#,
        r#"SELECT "a" FROM "t" ORDER BY "a" ASC NULLS LAST LIMIT 5"#,
    ));
    assert_eq!(v["queries"][0], v["queries"][1]);
    let v = ok(&pair(
        r#"SELECT "a" FROM "t" ORDER BY "a" DESC LIMIT 5"#,
        r#"SELECT "a" FROM "t" ORDER BY "a" DESC NULLS FIRST LIMIT 5"#,
    ));
    assert_eq!(v["queries"][0], v["queries"][1]);
}

/// The collation tuple is `(index, type, direction)` with no field for null placement, so placement
/// is folded into the direction tag. Without that fold these two mint the same symbol and prove
/// equal — a false proof, since they disagree on where the NULLs in `a` land.
#[test]
fn null_placement_reaches_the_collation() {
    let v = ok(&pair(
        r#"SELECT "a" FROM "t" ORDER BY "a" NULLS FIRST LIMIT 5"#,
        r#"SELECT "a" FROM "t" ORDER BY "a" NULLS LAST LIMIT 5"#,
    ));
    assert_ne!(v["queries"][0], v["queries"][1]);
}

/// Evaluation pops the collation from the end, so entry order fixes the `HOp` nesting and hence the
/// symbol. Canonicalising the order — sorting it to make more pairs match — would make these two
/// congruent, and they are not equivalent.
#[test]
fn collation_order_is_not_canonicalised() {
    let v = ok(&pair(
        r#"SELECT "a", "b" FROM "t" ORDER BY "a", "b" LIMIT 5"#,
        r#"SELECT "a", "b" FROM "t" ORDER BY "b", "a" LIMIT 5"#,
    ));
    assert_ne!(v["queries"][0], v["queries"][1]);
}

/// `ORDER BY 2` is a 1-based position in the select list.
#[test]
fn order_by_position_resolves_to_a_column() {
    let v =
        ok(&pair(r#"SELECT "b", "a" FROM "t" ORDER BY 2 LIMIT 5"#, r#"SELECT "b", "a" FROM "t""#));
    let s = find_with_field(&v["queries"][0], "collation").expect("a sort node");
    assert_eq!(s["collation"], serde_json::json!([[1, "INTEGER", "ASCENDING NULLS LAST"]]));
}

/// A wildcard needs no special case. `*` is already expanded into named columns by the time output
/// columns exist, so a key the star covers resolves like any other — which is why this is the one
/// place the emission is *less* restrictive than the syntactic pass in `normalize`.
#[test]
fn a_wildcard_projects_the_order_key() {
    let v = ok(&pair(r#"SELECT * FROM "t" ORDER BY "b" LIMIT 5"#, r#"SELECT * FROM "t""#));
    let s = find_with_field(&v["queries"][0], "collation").expect("a sort node");
    assert_eq!(s["collation"], serde_json::json!([[1, "VARCHAR", "ASCENDING NULLS LAST"]]));
}

/// Pagination nested in a derived table lowers too: `lower_query_ctx` recurses, so this was never a
/// top-level-only concern.
#[test]
fn pagination_nested_in_a_derived_table_lowers() {
    let v = ok(&pair(
        r#"SELECT "x" FROM (SELECT "a" AS "x" FROM "t" ORDER BY "x" LIMIT 5) AS "v""#,
        r#"SELECT "x" FROM (SELECT "a" AS "x" FROM "t") AS "v""#,
    ));
    let s = find_with_field(&v["queries"][0], "collation").expect("a sort node");
    assert_eq!(s["collation"], serde_json::json!([[0, "INTEGER", "ASCENDING NULLS LAST"]]));
}

/// A key the projection drops gets the source extended with it and the result trimmed back —
/// Calcite's `Project(trim) <- Sort <- Project(outputs ++ keys)`.
#[test]
fn an_order_key_the_projection_drops_is_appended_to_it() {
    let v = ok(&pair(r#"SELECT "a" FROM "t" ORDER BY "b" LIMIT 5"#, r#"SELECT "a" FROM "t""#));
    let q = &v["queries"][0];
    // The query still outputs one column…
    assert_eq!(q["project"]["target"].as_array().unwrap().len(), 1);
    // …but the projection under the Sort carries two, and the collation points at the added one.
    let sort = find_with_field(q, "collation").expect("a sort node");
    assert_eq!(sort["collation"], serde_json::json!([[1, "VARCHAR", "ASCENDING NULLS LAST"]]));
    assert_eq!(sort["source"]["project"]["target"].as_array().unwrap().len(), 2);
}

/// The same widening carries an arbitrary expression, which has no output position at all.
#[test]
fn an_expression_order_key_is_appended_too() {
    let v = ok(&pair(
        r#"SELECT "a" FROM "t" ORDER BY "a" + 1 DESC LIMIT 5"#,
        r#"SELECT "a" FROM "t""#,
    ));
    let sort = find_with_field(&v["queries"][0], "collation").expect("a sort node");
    assert_eq!(sort["collation"], serde_json::json!([[1, "INTEGER", "DESCENDING NULLS FIRST"]]));
    let added = &sort["source"]["project"]["target"][1];
    assert_eq!(added["operator"], "+");
}

/// The widening must not change what the query returns: the trim above the `Sort` puts the output
/// back to the columns the select declared, so a pair differing only in a dropped sort key is not
/// silently given an extra column.
#[test]
fn the_sandwich_leaves_the_output_columns_alone() {
    let v = ok(&pair(
        r#"SELECT "a", "b" FROM "t" ORDER BY "c" LIMIT 5"#,
        r#"SELECT "a", "b" FROM "t" ORDER BY "c" LIMIT 5"#,
    ));
    assert_eq!(v["queries"][0], v["queries"][1]);
    assert_eq!(v["queries"][0]["project"]["target"].as_array().unwrap().len(), 2);
}

/// A key already in the projection needs no widening, and the plain `Sort` shape is kept.
#[test]
fn a_resolvable_order_key_does_not_widen_the_projection() {
    let v = ok(&pair(r#"SELECT "a", "b" FROM "t" ORDER BY "b" LIMIT 5"#, r#"SELECT "a" FROM "t""#));
    // Sort on top, no trim above it.
    assert!(v["queries"][0].get("sort").is_some());
}

/// Two output columns of the same name is a resolution question, not a missing column: widening the
/// projection would not answer it, so it stays refused.
#[test]
fn refuses_an_ambiguous_order_key() {
    match lower_sql(&pair(
        r#"SELECT "a" AS "x", "b" AS "x" FROM "t" ORDER BY "x" LIMIT 5"#,
        r#"SELECT "a" FROM "t""#,
    )) {
        Err(FrontendError::Schema(m)) => assert!(m.contains("ambiguous ORDER BY key"), "{m}"),
        other => panic!("expected the ambiguity refusal, got {other:?}"),
    }
}

/// `USING >` is a descending sort; read as the ascending default it would prove equal to `ASC`.
#[test]
fn refuses_order_by_using() {
    refused(
        &pair(
            r#"SELECT "a" FROM "t" ORDER BY "a" USING > LIMIT 5"#,
            r#"SELECT "a" FROM "t" ORDER BY "a" ASC LIMIT 5"#,
        ),
        "USING",
    );
}

/// Nothing above a `Group` can address the FROM scope, so those keep the refusal rather than
/// appending a key at an index that means something else.
#[test]
fn refuses_to_widen_past_a_group() {
    for q in [
        r#"SELECT "a" FROM "t" GROUP BY "a" ORDER BY MAX("b") LIMIT 5"#,
        r#"SELECT DISTINCT "a" FROM "t" ORDER BY "b" LIMIT 5"#,
    ] {
        refused(&pair(q, r#"SELECT "a" FROM "t""#), "ORDER BY key is not an output column");
    }
}

/// A set operation has no single FROM scope to resolve a key against.
#[test]
fn refuses_to_widen_a_set_operation() {
    refused(
        &pair(
            r#"SELECT "a" FROM "t" UNION SELECT "a" FROM "t" ORDER BY "b" LIMIT 5"#,
            r#"SELECT "a" FROM "t""#,
        ),
        "ORDER BY key is not an output column",
    );
}

/// Forms whose row count is not the count in the clause, or is not known at all.
#[test]
fn refuses_pagination_it_cannot_express() {
    refused(
        &pair(
            r#"SELECT "a" FROM "t" ORDER BY "a" FETCH FIRST 2 ROWS WITH TIES"#,
            r#"SELECT "a" FROM "t""#,
        ),
        "WITH TIES",
    );
}

/// A count is lowered against an *empty* scope, so a column name in it is unresolved rather than
/// quietly read as a reference to the body's column.
#[test]
fn a_column_is_not_a_count() {
    let sql = pair(r#"SELECT "a" FROM "t" LIMIT "a""#, r#"SELECT "a" FROM "t""#);
    match lower_sql(&sql) {
        Err(FrontendError::Schema(m)) => assert!(m.contains("unresolved column a"), "{m:?}"),
        other => panic!("expected an unresolved-column schema error, got {other:?}"),
    }
}

/// The common shape in real logs: `LIMIT $N`. By the time lowering runs the parameter is the nullary
/// constant `qpN(0)`, and it belongs in the `Sort` as-is — restricting counts to literals would
/// refuse most of the pairs that carry one.
///
/// Run under `InferredSeeded` because that is the mode that substitutes parameters at all; the
/// default `Declared` mode refuses every `$N` before pagination is reached (see [`CatalogSource`]).
#[test]
fn a_parameter_is_a_count() {
    let sql = pair(
        r#"SELECT "a" FROM "t" ORDER BY "a" LIMIT $1 OFFSET $2"#,
        r#"SELECT "a" FROM "t""#,
    );
    let v = lower_with(&sql, CatalogSource::InferredSeeded)
        .unwrap_or_else(|e| panic!("expected Ok, got {e}"));
    let sort = find_with_field(&v, "sort").expect("a sort node").get("sort").unwrap().clone();
    assert_eq!(sort["limit"]["operator"], "QP1");
    assert_eq!(sort["offset"]["operator"], "QP2");
    // Distinct parameters stay distinct symbols, so mismatched pagination cannot prove.
    assert_ne!(sort["limit"], sort["offset"]);
}

/// A count is `INTEGER` even when the parameter's only *other* evidence says otherwise, and even when
/// it has none at all (which lands on `Ty::Opaque`, spelled `VARBINARY`).
///
/// This is the regression test for a z3 `SortDiffers` panic. One observed pair was `LIMIT $1`
/// against `LIMIT 1` — VARBINARY against Int — and another `LIMIT $5` against `LIMIT $4` with `$4`
/// unified into a boolean position. Congruence on the `limit` HOp asserts the two counts equal, so a
/// sort mismatch there is not an unproved pair, it is a crash.
#[test]
fn a_count_is_typed_integer_whatever_else_the_parameter_touches() {
    // `$1` is used as a boolean elsewhere, and as a count here. The count position wins.
    let sql = pair(
        r#"SELECT "a" FROM "t" WHERE $1 LIMIT $1"#,
        r#"SELECT "a" FROM "t" WHERE $1 LIMIT 1"#,
    );
    let v = lower_with(&sql, CatalogSource::InferredSeeded)
        .unwrap_or_else(|e| panic!("expected Ok, got {e}"));
    let sorts = collect_field(&v, "sort");
    assert_eq!(sorts.len(), 2, "both sides carry a slice");
    for s in &sorts {
        assert_eq!(s["limit"]["type"], "INTEGER", "count sort must match the literal's");
    }
}

#[test]
fn refuses_window_function() {
    let sql = format!("{T}\nSELECT COUNT(*) OVER () FROM \"t\";\nSELECT COUNT(*) FROM \"t\";");
    assert!(matches!(lower_sql(&sql), Err(FrontendError::Unsupported(_))));
}

#[test]
fn requires_exactly_two_queries() {
    let sql = format!("{T}\nSELECT \"a\" FROM \"t\";");
    assert!(matches!(lower_sql(&sql), Err(FrontendError::Schema(m)) if m.contains("2 queries")));
}

#[test]
fn correlated_subquery_uses_outer_offset() {
    // The correlated `t2` references the outer `t1.a`. Inner column indices are offset past the
    // outer row width (3 columns of t1), and the correlated ref uses the outer index 0.
    let sql = format!(
        "{T}\nSELECT \"a\" FROM \"t\" AS \"t1\" WHERE EXISTS (SELECT 1 FROM \"t\" AS \"t2\" WHERE \"t2\".\"a\" = \"t1\".\"a\");\n\
         SELECT \"a\" FROM \"t\" AS \"t1\" WHERE EXISTS (SELECT 1 FROM \"t\" AS \"t2\" WHERE \"t2\".\"a\" = \"t1\".\"a\");"
    );
    let v = ok(&sql);
    let s = v.to_string();
    // inner t2.a at offset 3 (= outer width), outer t1.a at 0
    assert!(s.contains("\"column\":3"), "inner column should be offset by the outer row width");
    assert_eq!(v["queries"][0], v["queries"][1]);
}

// ---------------------------------------------------------------------------------------------
// Row-constructor comparison
// ---------------------------------------------------------------------------------------------

#[test]
fn row_equality_is_the_pairwise_conjunction() {
    // The standard defines row `=` as the conjunction of the pairwise comparisons, three-valued
    // logic included. For two columns both spellings build the same flat 2-operand `AND`, so the
    // expansion is checked by equality against the hand-written conjunction rather than by shape.
    let v = ok(&pair(
        r#"SELECT "a" FROM "t" WHERE ("a", "b") = (1, 'x')"#,
        r#"SELECT "a" FROM "t" WHERE "a" = 1 AND "b" = 'x'"#,
    ));
    assert_eq!(v["queries"][0], v["queries"][1]);
}

#[test]
fn row_inequality_is_the_negated_conjunction() {
    // Row `<>` is defined as the negation of row `=`, not as the pairwise `<>`.
    let v = ok(&pair(
        r#"SELECT "a" FROM "t" WHERE ("a", "b") <> (1, 'x')"#,
        r#"SELECT "a" FROM "t" WHERE NOT ("a" = 1 AND "b" = 'x')"#,
    ));
    assert_eq!(v["queries"][0], v["queries"][1]);
}

#[test]
fn refuses_ordering_row_comparison() {
    // `<` on rows is lexicographic, so the pairwise expansion would be wrong; it must be refused
    // rather than expanded like `=`.
    refused(&same(r#"SELECT "a" FROM "t" WHERE ("a", "b") < (1, 'x')"#), "row comparison");
}

#[test]
fn rejects_row_comparison_arity_mismatch() {
    let sql = same(r#"SELECT "a" FROM "t" WHERE ("a", "b") = (1, 'x', 2)"#);
    assert!(matches!(lower_sql(&sql), Err(FrontendError::Schema(m)) if m.contains("arity")));
}

#[test]
fn row_in_subquery_keeps_one_operand_per_column() {
    let v = ok(&same(
        r#"SELECT "a" FROM "t" WHERE ("a", "b") IN (SELECT "a", "b" FROM "t")"#,
    ));
    let node = find_op(&v["queries"][0], "IN").expect("expected an IN node");
    assert_eq!(node["operand"].as_array().unwrap().len(), 2);
    assert!(node.get("query").is_some(), "the IN must carry its subquery relation");
}

// ---------------------------------------------------------------------------------------------
// FROM-less SELECT
// ---------------------------------------------------------------------------------------------

#[test]
fn fromless_select_lowers_to_values() {
    // One row, no input relation.
    let v = ok(&same("SELECT 1, 'x'"));
    let q = &v["queries"][0];
    assert!(q.get("values").is_some(), "a FROM-less SELECT should lower to Values, got {q}");
    assert_eq!(q["values"]["content"].as_array().unwrap().len(), 1, "exactly one row");
    assert_eq!(q["values"]["schema"], serde_json::json!(["INTEGER", "VARCHAR"]));
}

#[test]
fn refuses_fromless_select_with_a_where() {
    // Every remaining clause needs a source row to mean anything, and none is degenerate enough to
    // simply drop.
    refused(&same("SELECT 1 WHERE 1 = 1"), "FROM-less SELECT with WHERE");
}

#[test]
fn refuses_fromless_select_over_a_column() {
    // The prover evaluates a `Values` row's content one level *above* the row itself, so a
    // correlated column here would need different numbering than everywhere else in the frontend.
    refused(&same(r#"SELECT "a""#), "FROM-less SELECT over a column or subquery");
}

#[test]
fn refuses_fromless_select_over_a_subquery() {
    refused(&same(r#"SELECT (SELECT "a" FROM "t")"#), "FROM-less SELECT over a column or subquery");
}

#[test]
fn refuses_aggregate_in_a_fromless_select() {
    // No rows to fold over; without this guard it would be emitted into `Values` as if it were a
    // scalar.
    refused(&same("SELECT COUNT(*)"), "aggregate in a FROM-less SELECT");
}

// ---------------------------------------------------------------------------------------------
// Soundness guards: set-returning functions, table-factor modifiers, LATERAL
// ---------------------------------------------------------------------------------------------

#[test]
fn refuses_set_returning_function_in_scalar_position() {
    // Unknown functions are lowered as uninterpreted scalars, which is faithful because a function
    // returns one value. A set-returning function returns one row per element, so modelling it as a
    // scalar understates the cardinality.
    refused(&same(r#"SELECT EXPLODE("a") FROM "t""#), "set-returning function EXPLODE");
}

#[test]
fn refuses_tablesample() {
    // TABLESAMPLE is not even deterministic, so it cannot be dropped.
    refused(&same(r#"SELECT "a" FROM "t" TABLESAMPLE BERNOULLI (50)"#), "TABLESAMPLE");
}

#[test]
fn refuses_with_ordinality() {
    // Adds a row-number column, so the output shape differs from the bare scan.
    refused(&same(r#"SELECT "a" FROM "t" WITH ORDINALITY"#), "WITH ORDINALITY");
}

#[test]
fn refuses_lateral_derived_table() {
    // A LATERAL derived table may see its FROM siblings. A reference to one would usually just fail
    // to resolve, but not if a sibling shared an alias with an enclosing binding: SQL resolves that
    // to the sibling and we would silently reach the outer one instead.
    refused(
        &same(r#"SELECT "t1"."a" FROM "t" AS "t1", LATERAL (SELECT "a" FROM "t") AS "t2""#),
        "LATERAL derived table",
    );
}

// ---------------------------------------------------------------------------------------------
// Aggregate FILTER
// ---------------------------------------------------------------------------------------------

#[test]
fn agg_filter_becomes_a_case_over_the_argument() {
    // `agg(x) FILTER (WHERE p)` is rewritten to `agg(CASE WHEN p THEN x END)`: the fold sees a NULL
    // for every non-matching row and `ignoreNulls` then drops exactly those rows. Only valid for
    // aggregates that skip NULL inputs, which is why it is restricted to the builtins.
    let v = ok(&pair(
        r#"SELECT SUM("a") FILTER (WHERE "a" > 0) FROM "t""#,
        r#"SELECT SUM(CASE WHEN "a" > 0 THEN "a" END) FROM "t""#,
    ));
    assert_eq!(v["queries"][0], v["queries"][1]);
}

#[test]
fn count_star_filter_counts_a_literal_one() {
    // `COUNT(*)` has no argument to push the predicate into, so `1` stands in for the row.
    let v = ok(&pair(
        r#"SELECT COUNT(*) FILTER (WHERE "a" > 0) FROM "t""#,
        r#"SELECT COUNT(CASE WHEN "a" > 0 THEN 1 END) FROM "t""#,
    ));
    assert_eq!(v["queries"][0], v["queries"][1]);
}

#[test]
fn agg_filter_sets_ignore_nulls() {
    // The rewrite is only an identity because the NULLs it injects are then skipped. If this flag
    // were false the non-matching rows would be counted instead of dropped.
    let v = ok(&same(r#"SELECT COUNT("a") FILTER (WHERE "a" > 0) FROM "t""#));
    let f = find_with_field(&v["queries"][0], "ignoreNulls").expect("expected an AggCall");
    assert_eq!(f["ignoreNulls"], serde_json::json!(true), "in {f}");
    // The predicate went into the argument, so the fold sees a CASE rather than the bare column.
    assert_eq!(f["operator"], serde_json::json!("COUNT"));
}

#[test]
fn refuses_filter_on_a_declared_aggregate() {
    // A declared aggregate's null handling is unknown to us: `array_agg` *keeps* NULLs, so the CASE
    // rewrite would feed it one value per non-matching row instead of dropping the row.
    let ddl = r#"create table "t" ("a" INTEGER);
                 declare aggregate function my_agg(integer) returns VARCHAR;"#;
    let q = r#"SELECT my_agg("a") FILTER (WHERE "a" > 0) FROM "t""#;
    refused(&format!("{ddl}\n{q};\n{q};"), "FILTER on the non-builtin aggregate MY_AGG");
}

// ---------------------------------------------------------------------------------------------
// NULL is a typed constant, and CASE parity
// ---------------------------------------------------------------------------------------------

#[test]
fn null_branch_is_relabelled_not_cast() {
    // NULL is a typed nullary constant to the prover (`Op("NULL", [], ty)`), recognised as null by
    // comparison against the constant of that same type. Casting rather than relabelling would give
    // `CAST(NULL_INTEGER AS REAL)` = `int_to_real(NULL_INTEGER)`, which is no longer *equal* to
    // `NULL_REAL` and so stops testing as null -- breaking `IS NULL` and the aggregates' null
    // skipping through a CASE branch.
    let v = ok(&same(r#"SELECT CASE WHEN "a" > 0 THEN 1.5 ELSE NULL END FROM "t""#));
    let q = v["queries"][0].to_string();
    assert!(
        q.contains(r#"{"operand":[],"operator":"NULL","type":"REAL"}"#),
        "the NULL branch should be relabelled REAL, got {q}"
    );
    assert!(!q.contains(r#""operator":"CAST""#), "the NULL branch must not be CAST, got {q}");
}

#[test]
fn case_without_else_keeps_an_odd_operand_count() {
    // The prover reads the two CASE forms off the operand-count parity: odd is the searched form
    // `[cond, body]*, else`, **even is the simple form** `input, [val, body]*, else`. An even list
    // is therefore not "searched CASE with no ELSE" -- operand 0 is silently reinterpreted as a
    // scrutinee, and a 2-operand CASE evaluates to just operand 1.
    let v = ok(&same(r#"SELECT CASE WHEN "a" > 0 THEN 1 END FROM "t""#));
    let node = find_op(&v["queries"][0], "CASE").expect("expected a CASE node");
    let n = node["operand"].as_array().unwrap().len();
    assert_eq!(n % 2, 1, "CASE operand count must be odd, got {n} in {node}");
    assert_eq!(n, 3, "the absent ELSE should be an appended NULL");
}

// ---------------------------------------------------------------------------------------------
// Scalar subquery
// ---------------------------------------------------------------------------------------------

#[test]
fn scalar_subquery_lowers_to_a_scalar_query_hop() {
    // `$SCALAR_QUERY` is the spelling Calcite's parser uses. The prover reads it as a higher-order
    // operator: an uninterpreted value keyed on the normalised relation and the outer row.
    let v = ok(&same(r#"SELECT "a" FROM "t" WHERE "a" > (SELECT MAX("a") FROM "t")"#));
    let node = find_op(&v["queries"][0], "$SCALAR_QUERY").expect("expected a $SCALAR_QUERY node");
    assert!(node.get("query").is_some(), "must carry its subquery relation");
    assert_eq!(node["operand"], serde_json::json!([]));
    assert_eq!(node["type"], serde_json::json!("INTEGER"), "type comes from the selected column");
}

#[test]
fn distinct_scalar_subqueries_lower_distinctly() {
    // The soundness-critical property. The prover's HOp memo key includes the relation, so two
    // subqueries collapse to one value exactly when their relations normalise equal. If they
    // lowered to the same IR here, `MAX` and `MIN` would be equated and this pair would be
    // "proved".
    let v = ok(&pair(
        r#"SELECT "a" FROM "t" WHERE "a" > (SELECT MAX("a") FROM "t")"#,
        r#"SELECT "a" FROM "t" WHERE "a" > (SELECT MIN("a") FROM "t")"#,
    ));
    assert_ne!(v["queries"][0], v["queries"][1]);
}

#[test]
fn correlated_scalar_subquery_resolves_the_outer_column() {
    // Correlating on `a` and on `b` must not lower alike -- the correlated reference is part of the
    // relation, hence of the value's identity.
    let v = ok(&pair(
        r#"SELECT "a" FROM "t" AS "o" WHERE "a" > (SELECT MAX("i"."a") FROM "t" AS "i" WHERE "i"."a" > "o"."a")"#,
        r#"SELECT "a" FROM "t" AS "o" WHERE "a" > (SELECT MAX("i"."a") FROM "t" AS "i" WHERE "i"."a" > "o"."b")"#,
    ));
    assert_ne!(v["queries"][0], v["queries"][1]);
}

#[test]
fn rejects_multi_column_scalar_subquery() {
    let sql = same(r#"SELECT "a" FROM "t" WHERE "a" > (SELECT "a", "b" FROM "t")"#);
    assert!(matches!(lower_sql(&sql), Err(FrontendError::Schema(m)) if m.contains("expected 1")));
}

// ---------------------------------------------------------------------------
// Function name resolution and the undeclared-call default (task #20).
// ---------------------------------------------------------------------------

#[test]
fn a_declare_reaches_a_qualified_call() {
    // A `declare` names a *function*, not a call site. The preprocessor makes this concrete:
    // sqlglot writes the declare under the bare `like_escape` but renders the call qualified, so
    // keying only on the qualified spelling silently drops the declaration and the call falls back
    // to the default type.
    let sql = format!(
        "{T}\ndeclare scalar function like_escape(VARCHAR, VARCHAR) returns VARCHAR;\n\
         SELECT \"pg_catalog\".like_escape(\"b\", \"b\") FROM \"t\";\n\
         SELECT \"pg_catalog\".like_escape(\"b\", \"b\") FROM \"t\";"
    );
    let v = ok(&sql);
    let call = find_op(&v, "PG_CATALOG.LIKE_ESCAPE").expect("the qualified call is in the IR");
    assert_eq!(call["type"], "VARCHAR", "the declared return type must reach a qualified call");
}

#[test]
fn a_qualified_call_keeps_its_qualifier_as_its_identity() {
    // The declaration lookup falls back to the bare name, but the *symbol* must not: collapsing
    // `sales.total` and `hr.total` into one `TOTAL` would let the prover assume two different
    // functions are the same function, which is a false-proof channel.
    let v = ok(&pair(
        r#"SELECT "sales".total("a") FROM "t""#,
        r#"SELECT "hr".total("a") FROM "t""#,
    ));
    assert_ne!(v["queries"][0], v["queries"][1], "different schemas are different functions");
    assert!(find_op(&v["queries"][0], "SALES.TOTAL").is_some());
    assert!(find_op(&v["queries"][1], "HR.TOTAL").is_some());
}

#[test]
fn an_exact_declaration_beats_the_bare_fallback() {
    let sql = format!(
        "{T}\ndeclare scalar function f(VARCHAR) returns INTEGER;\n\
         declare scalar function s.f(VARCHAR) returns REAL;\n\
         SELECT \"s\".f(\"b\") FROM \"t\";\nSELECT \"s\".f(\"b\") FROM \"t\";"
    );
    let v = ok(&sql);
    assert_eq!(find_op(&v, "S.F").expect("the call")["type"], "REAL");
}

#[test]
fn a_qualified_declared_aggregate_is_still_an_aggregate() {
    // SOUNDNESS: missing the declaration here sends the call down the scalar path, where it becomes
    // a per-row function and turns one output row into one row per input row.
    let sql = format!(
        "{T}\ndeclare aggregate function qa_total(INTEGER) returns INTEGER;\n\
         SELECT \"s\".qa_total(\"a\") FROM \"t\";\nSELECT \"s\".qa_total(\"a\") FROM \"t\";"
    );
    let v = ok(&sql);
    assert!(
        v["queries"][0].get("group").is_some(),
        "a qualified declared aggregate must reach the Group path, not the scalar one"
    );
}

#[test]
fn a_qualified_builtin_aggregate_is_refused() {
    // Neither reading is safe: `pg_catalog.sum` is the builtin, `myschema.sum` may be anything.
    // Assuming the builtin asserts real summation; assuming a scalar inflates the row count.
    refused(&same(r#"SELECT "s".sum("a") FROM "t""#), "qualified builtin aggregate");
}

#[test]
fn a_qualified_set_returning_function_is_still_refused() {
    // `public.unnest(x)` is still `unnest`; the blocklist is matched on the bare name because
    // widening a refusal can only ever cost completeness.
    refused(&same(r#"SELECT "public".unnest("a") FROM "t""#), "set-returning function");
}

#[test]
fn an_undeclared_call_is_opaque_not_an_integer() {
    // The default has to be the faithful one whichever type the function really returns. An opaque
    // result supports equality and nothing else; INTEGER would hand the prover integer arithmetic
    // and a total order over a value that is just as likely a timestamp or a string.
    let v = ok(&same(r#"SELECT timestamp_trunc("a", "b") FROM "t""#));
    let call = find_op(&v, "TIMESTAMP_TRUNC").expect("the undeclared call is in the IR");
    assert_eq!(call["type"], "VARBINARY");
}

#[test]
fn arithmetic_on_an_undeclared_result_stays_uninterpreted() {
    // The consequence of the line above, and the reason it matters: `f(x) + 1` must not become
    // integer addition, because the prover would then apply integer laws to whatever `f` returns.
    let v = ok(&same(r#"SELECT timestamp_trunc("a", "b") + 1 FROM "t""#));
    let plus = find_op(&v, "+").expect("the addition is in the IR");
    assert_eq!(plus["type"], "VARBINARY", "opaque operand must make the result opaque");
}

// Aggregates the prover does not model, and calls that are not functions (task #21).

#[test]
fn an_unmodelled_aggregate_is_grouped_not_demoted() {
    // `bool_or` is in neither the native list nor any `declare` line. Matching nothing used to mean
    // the query never took the Group path and the call became a per-row scalar — one output row
    // turned into one row per input row.
    let v = ok(&same(r#"SELECT bool_or("a" > 1) FROM "t""#));
    assert!(
        v["queries"][0].get("group").is_some(),
        "bool_or must reach the Group path, not the scalar one"
    );
}

#[test]
fn bool_and_and_every_are_aggregates_too() {
    for q in [r#"SELECT bool_and("a" > 1) FROM "t""#, r#"SELECT every("a" > 1) FROM "t""#] {
        assert!(ok(&same(q))["queries"][0].get("group").is_some(), "{q} must be an aggregate");
    }
}

#[test]
fn an_unmodelled_aggregate_returns_its_own_type() {
    // Falling through to "the type of the first argument" would be wrong here for the same reason
    // the undeclared-call default is: it asserts something about a function nobody described. That
    // `bool_or` returns a boolean is definitional, not a guess.
    let v = ok(&same(r#"SELECT bool_or("a" > 1) FROM "t""#));
    let call = find_op(&v, "BOOL_OR").expect("the aggregate is in the IR");
    assert_eq!(call["type"], "BOOLEAN");
}

#[test]
fn an_unmodelled_aggregate_does_not_claim_to_skip_nulls() {
    // `ignoreNulls` is an assertion about null handling, and we are not modelling this aggregate —
    // `false` distinguishes bags that differ only in NULLs rather than conflating them.
    let v = ok(&same(r#"SELECT bool_or("a" > 1) FROM "t""#));
    let call = find_op(&v, "BOOL_OR").expect("the aggregate is in the IR");
    assert_eq!(call["ignoreNulls"], false);
}

#[test]
fn filter_on_an_unmodelled_aggregate_is_refused() {
    // The FILTER rewrite (`agg(CASE WHEN p THEN x END)`) only says what FILTER says when the
    // aggregate drops the NULLs it feeds in, and `ignoreNulls` above is exactly what we withhold.
    refused(
        &same(r#"SELECT bool_or("a" > 1) FILTER (WHERE "a" > 2) FROM "t""#),
        "FILTER on the non-builtin aggregate",
    );
}

#[test]
fn a_qualified_unmodelled_aggregate_is_refused() {
    // Same argument as the native ones: `myschema.bool_or` may be anything, and neither reading of
    // it is safe to assume.
    refused(&same(r#"SELECT "s".bool_or("a" > 1) FROM "t""#), "qualified builtin aggregate");
}

#[test]
fn a_qualified_declared_aggregate_gets_its_declared_return_type() {
    // `is_agg_call` finds the declaration through the bare name; the *return type* lookup has to
    // reach it the same way, or the aggregate is typed as its argument instead.
    let sql = format!(
        "{T}\ndeclare aggregate function qa_total(INTEGER) returns VARCHAR;\n\
         SELECT \"s\".qa_total(\"a\") FROM \"t\";\nSELECT \"s\".qa_total(\"a\") FROM \"t\";"
    );
    let v = ok(&sql);
    let call = find_op(&v, "S.QA_TOTAL").expect("the aggregate is in the IR");
    assert_eq!(call["type"], "VARCHAR", "the declared return type must reach a qualified call");
}

#[test]
fn an_order_sensitive_aggregate_is_refused() {
    // The line between these and `bool_or` above is whether the bag determines the result. Disjunction
    // is commutative, associative and idempotent, so it does; `array_agg` over {a, b} is `[a, b]` or
    // `[b, a]` depending on an order SQL leaves unspecified, so it does not. An uninterpreted
    // aggregate symbol *is* a function of the bag, so modelling one of these as one asserts an
    // equality Postgres does not honour — the unsound direction.
    for f in [
        "array_agg(\"a\")",
        "string_agg(\"b\", ',')",
        "json_agg(\"a\")",
        "jsonb_agg(\"a\")",
        "group_concat(\"b\")",
    ] {
        refused(&same(&format!(r#"SELECT {f} FROM "t""#)), "order-sensitive aggregate");
    }
}

#[test]
fn an_order_sensitive_aggregate_is_refused_in_scalar_position_too() {
    // Matching no aggregate list is how these used to reach the *scalar* path, where the defect is a
    // different one: `SELECT array_agg(v) FROM t` became a per-row call and returned one row per input
    // row instead of exactly one. Both positions are refused, so neither reading can come back.
    refused(&same(r#"SELECT "a" FROM "t" WHERE "b" = string_agg("b", ',')"#), "order-sensitive aggregate");
}

#[test]
fn a_declaration_does_not_license_an_order_sensitive_aggregate() {
    // A `declare aggregate function` line makes `is_agg_call` true and routes the call down the Group
    // path, past both call-lowering guards. It says what the function returns; it does not say that
    // the bag determines the value.
    let sql = format!(
        "{T}\ndeclare aggregate function array_agg(VARCHAR) returns VARBINARY;\n\
         SELECT array_agg(\"b\") FROM \"t\";\nSELECT array_agg(\"b\") FROM \"t\";"
    );
    refused(&sql, "order-sensitive aggregate");
}

#[test]
fn a_qualified_order_sensitive_aggregate_is_still_refused() {
    refused(&same(r#"SELECT "pg_catalog".array_agg("a") FROM "t""#), "order-sensitive aggregate");
}

#[test]
fn a_non_deterministic_function_is_refused() {
    // An uninterpreted *function* asserts that equal arguments give equal results, which is what
    // lets both sides of a rewrite share one symbol. These do not have that property.
    for f in ["random()", "gen_random_uuid()", "nextval('s')", "clock_timestamp()"] {
        refused(&same(&format!(r#"SELECT {f} FROM "t""#)), "non-deterministic function");
    }
}

#[test]
fn a_non_deterministic_call_is_refused_in_argument_position() {
    // The refusal has to be at the call, not at the projection: the damage is the same wherever the
    // symbol appears.
    refused(&same(r#"SELECT "a" FROM "t" WHERE "a" < random()"#), "non-deterministic function");
}

#[test]
fn a_qualified_non_deterministic_function_is_still_refused() {
    // Matched on the bare name — `pg_catalog.random` is still `random`, and widening a refusal can
    // only ever cost completeness.
    refused(&same(r#"SELECT "pg_catalog".random() FROM "t""#), "non-deterministic function");
}

#[test]
fn a_statement_stable_clock_is_not_refused() {
    // `now()` and friends are fixed for the duration of a statement, so a shared constant is
    // faithful. Refusing them would cost coverage for nothing.
    for q in [r#"SELECT now() FROM "t""#, r#"SELECT current_timestamp FROM "t""#] {
        ok(&same(q));
    }
}

// Quantified comparison: `= ANY` and `<> ALL` over each operand shape (task #22).

#[test]
fn eq_any_over_a_subquery_is_exactly_in() {
    // Not "equivalent to" -- the same predicate, so it had better be the same IR. Sharing the
    // lowering is also what makes a rewrite between the two spellings provable.
    let v = ok(&pair(
        r#"SELECT "a" FROM "t" WHERE "a" IN (SELECT "a" FROM "t")"#,
        r#"SELECT "a" FROM "t" WHERE "a" = ANY (SELECT "a" FROM "t")"#,
    ));
    assert_eq!(v["queries"][0], v["queries"][1]);
}

#[test]
fn ne_all_over_a_subquery_is_exactly_not_in() {
    let v = ok(&pair(
        r#"SELECT "a" FROM "t" WHERE "a" NOT IN (SELECT "a" FROM "t")"#,
        r#"SELECT "a" FROM "t" WHERE "a" <> ALL (SELECT "a" FROM "t")"#,
    ));
    assert_eq!(v["queries"][0], v["queries"][1]);
}

#[test]
fn some_is_a_spelling_of_any() {
    let v = ok(&pair(
        r#"SELECT "a" FROM "t" WHERE "a" = ANY (SELECT "a" FROM "t")"#,
        r#"SELECT "a" FROM "t" WHERE "a" = SOME (SELECT "a" FROM "t")"#,
    ));
    assert_eq!(v["queries"][0], v["queries"][1]);
}

#[test]
fn parentheses_around_the_operand_do_not_hide_the_subquery() {
    // `ANY((SELECT ..))` must still take the relation path, not fall through to the opaque one.
    let v = ok(&pair(
        r#"SELECT "a" FROM "t" WHERE "a" IN (SELECT "a" FROM "t")"#,
        r#"SELECT "a" FROM "t" WHERE "a" = ANY ((SELECT "a" FROM "t"))"#,
    ));
    assert_eq!(v["queries"][0], v["queries"][1]);
}

#[test]
fn eq_any_over_an_array_literal_becomes_an_or_chain() {
    let v = ok(&same(r#"SELECT "a" FROM "t" WHERE "a" = ANY (ARRAY[1, 2, 3])"#));
    let or = find_op(&v, "OR").expect("the disjunction is in the IR");
    assert_eq!(or["operand"].as_array().unwrap().len(), 3);
    assert!(find_op(&v, "= ANY").is_none(), "an element list must be expanded, not left opaque");
}

#[test]
fn ne_all_over_an_array_literal_becomes_an_and_chain_of_disequalities() {
    let v = ok(&same(r#"SELECT "a" FROM "t" WHERE "a" <> ALL (ARRAY[1, 2])"#));
    let and = find_op(&v, "AND").expect("the conjunction is in the IR");
    let ops = and["operand"].as_array().unwrap();
    assert_eq!(ops.len(), 2);
    assert!(ops.iter().all(|o| o["operator"] == "<>"), "ALL negates each comparison");
}

#[test]
fn a_one_element_array_needs_no_connective() {
    let v = ok(&same(r#"SELECT "a" FROM "t" WHERE "a" = ANY (ARRAY[7])"#));
    assert!(find_op(&v, "OR").is_none(), "a single disjunct is the predicate itself");
    assert!(find_op(&v, "=").is_some());
}

#[test]
fn eq_any_over_an_opaque_operand_is_one_uninterpreted_symbol() {
    // An array-valued column or parameter has no element list and no relation. It becomes a free
    // boolean function of both operands -- sound because it *is* a function of them.
    let v = ok(&same(r#"SELECT "a" FROM "t" WHERE "a" = ANY ("c")"#));
    let n = find_op(&v, "= ANY").expect("the quantified comparison is in the IR");
    assert_eq!(n["operand"].as_array().unwrap().len(), 2, "applied to both operands");
    assert_eq!(n["type"], "BOOLEAN");
}

// `IN (SELECT unnest(A))` -> `= ANY(A)`, the normalization in `normalize::unnest_in_to_any`.

#[test]
fn unnest_in_and_eq_any_lower_to_the_same_ir() {
    // The real-world shape this normalization exists for. Before it, the right-hand spelling refused
    // on `unnest` in scalar position, leaving pairs undecided for a purely syntactic reason.
    let v = ok(&pair(
        r#"SELECT "a" FROM "t" WHERE "a" = ANY ("c")"#,
        r#"SELECT "a" FROM "t" WHERE "a" IN (SELECT unnest("c"))"#,
    ));
    assert_eq!(v["queries"][0], v["queries"][1]);
    assert!(find_op(&v["queries"][1], "= ANY").is_some());
}

#[test]
fn unnest_in_over_an_array_literal_takes_the_expanding_arm() {
    // Once rewritten it is an ordinary `= ANY`, so an element list expands rather than going opaque.
    // Exact including NULLs and the empty array, and an array *literal* can never itself be NULL,
    // so the NULL-array divergence the polarity guard exists for cannot arise here.
    let v = ok(&same(r#"SELECT "a" FROM "t" WHERE "a" IN (SELECT unnest(ARRAY[1, 2, 3]))"#));
    assert_eq!(find_op(&v, "OR").expect("the disjunction is in the IR")["operand"]
                   .as_array().unwrap().len(), 3);
    assert!(find_op(&v, "= ANY").is_none());
}

#[test]
fn unnest_in_outside_a_positive_filter_is_still_refused() {
    // The guard's whole point. `x = ANY(NULL::int[])` is NULL and `x IN (SELECT unnest(NULL::int[]))`
    // is FALSE, so anywhere the difference is observable the rewrite must not fire -- and then the
    // original refusal stands, which is the safe outcome.
    // Spelled with `$1` rather than a column so the refusal reported is the `unnest` one: a
    // FROM-less `SELECT unnest("c")` is *correlated*, and its own separate guard fires first.
    for q in [
        r#"SELECT "a" IN (SELECT unnest($1)) FROM "t""#,
        r#"SELECT "a" FROM "t" WHERE NOT ("a" IN (SELECT unnest($1)))"#,
        r#"SELECT "a" FROM "t" WHERE ("a" IN (SELECT unnest($1))) IS NULL"#,
        r#"SELECT "a" FROM "t" WHERE "a" NOT IN (SELECT unnest($1))"#,
        r#"SELECT "a" FROM "t" WHERE NOT ("a" IN (SELECT unnest(ARRAY[1, 2])))"#,
    ] {
        refused(&same(q), "set-returning function");
    }
}

#[test]
fn eq_any_and_ne_all_do_not_share_a_symbol() {
    // The quantifier and the comparison operator are both part of the symbol's identity; if they
    // were not, an uninterpreted `= ANY` could unify with its own negation.
    let v = ok(&pair(
        r#"SELECT "a" FROM "t" WHERE "a" = ANY ("c")"#,
        r#"SELECT "a" FROM "t" WHERE "a" <> ALL ("c")"#,
    ));
    assert!(find_op(&v["queries"][0], "= ANY").is_some());
    assert!(find_op(&v["queries"][1], "<> ALL").is_some());
    assert_ne!(v["queries"][0], v["queries"][1]);
}

#[test]
fn an_ordered_quantified_comparison_is_refused() {
    // A prover-panic guard, not a completeness choice: `quant_cmp` reaches `fn cmp` for these, and
    // that function asserts the operand type is Integer|Real|String without checking on the way in.
    for q in [
        r#"SELECT "a" FROM "t" WHERE "a" > ANY (SELECT "a" FROM "t")"#,
        r#"SELECT "a" FROM "t" WHERE "a" <= ALL (SELECT "a" FROM "t")"#,
    ] {
        refused(&same(q), "only `= ANY` and `<> ALL` are lowered");
    }
}

#[test]
fn the_other_two_quantifier_pairings_are_refused() {
    for q in [
        r#"SELECT "a" FROM "t" WHERE "a" = ALL (SELECT "a" FROM "t")"#,
        r#"SELECT "a" FROM "t" WHERE "a" <> ANY (SELECT "a" FROM "t")"#,
    ] {
        refused(&same(q), "only `= ANY` and `<> ALL` are lowered");
    }
}

#[test]
fn a_row_valued_quantified_comparison_checks_its_arity() {
    // Same guard as the `IN` arm, and for the same reason: the prover asserts the widths match.
    let e = lower_sql(&same(
        r#"SELECT "a" FROM "t" WHERE ("a", "b") = ANY (SELECT "a" FROM "t")"#,
    ))
    .expect_err("a width mismatch must not reach the prover");
    assert!(e.to_string().contains("arity"), "expected an arity refusal, got {e}");
}

#[test]
fn an_aggregate_under_a_quantified_comparison_is_refused_not_demoted() {
    // `contains_agg` does not walk into these nodes, so this arrives on the scalar path. The
    // function arm has to catch it: lowering `count` as a per-row call would silently turn one
    // output row into one row per input row.
    refused(&same(r#"SELECT count(*) = ANY ("c") FROM "t""#), "in scalar position");
}

#[test]
fn an_empty_array_is_the_connective_identity() {
    // SQL agrees with the empty OR / empty AND: nothing to match means `= ANY` is FALSE, and
    // vacuously means `<> ALL` is TRUE. Both hold even when the left side is NULL, which is why
    // this is the one shape a strictness assumption would have got wrong.
    let f = ok(&same(r#"SELECT "a" FROM "t" WHERE "a" = ANY (ARRAY[])"#));
    assert!(find_op(&f, "FALSE").is_some(), "`= ANY` of nothing is FALSE");
    let t = ok(&same(r#"SELECT "a" FROM "t" WHERE "a" <> ALL (ARRAY[])"#));
    assert!(find_op(&t, "TRUE").is_some(), "`<> ALL` of nothing is TRUE");
}

// Variable numbering inside a subquery (task #23). The prover addresses columns by an absolute
// level into the row it is evaluating, and a subquery's row starts *after* the enclosing one. That
// is automatic for anything resolved through a FROM binding, which carries its own offset, and it
// is not automatic for the binders the frontend introduces itself — the pre-aggregation projection,
// the group output, and the projection under `SELECT DISTINCT`. Writing a bare `0..n` there reaches
// enclosing columns instead, and at the top level (where the offset is zero) that mistake is
// invisible; these pin it where it is visible.
//
// Every one of these would also be caught by the check `lower_sql` now runs over its own output, so
// they are as much about saying what the right number *is* as about catching the wrong one.

// The DISTINCT sits in a derived table rather than directly on the `IN` subquery, because
// `normalize::strip_in_exists_distinct` removes it in the latter position — a `DISTINCT` there is
// unobservable, which is exactly why it is stripped. One level deeper it is observable, it survives,
// and the offsets it has to get right are the same ones.
#[test]
fn distinct_in_a_subquery_keys_its_group_past_the_outer_row() {
    let v = ok(&same(
        r#"SELECT "a" FROM "t" WHERE "b" IN (SELECT "x" FROM (SELECT DISTINCT "b" AS "x" FROM "t") "s")"#,
    ));
    let sub = &find_op(&v, "IN").expect("an IN node")["query"];
    let g = &find_with_field(sub, "group").expect("a Group node")["group"];
    // The outer `t` occupies levels 0..3, so the subquery's own projection starts at 3.
    assert_eq!(g["keys"][0]["column"], 3);
    assert_eq!(g["keys"][0]["type"], "VARCHAR");
    // ...and that projection reads the inner `t`'s `b`, which is level 3 + 1.
    assert_eq!(g["source"]["project"]["target"][0]["column"], 4);
}

#[test]
fn an_aggregate_in_a_subquery_numbers_its_group_past_the_outer_row() {
    let v = ok(&same(r#"SELECT "a" FROM "t" WHERE "a" IN (SELECT count("b") FROM "t" GROUP BY "a")"#));
    let sub = &find_op(&v, "IN").expect("an IN node")["query"];
    let g = &find_with_field(sub, "group").expect("a Group node")["group"];
    // The group reads the pre-aggregation projection `[a, b]`, which begins at level 3.
    assert_eq!(g["keys"][0]["column"], 3);
    assert_eq!(g["function"][0]["operand"][0]["column"], 4);
    // The projection over the group output reads the count, one past the single key.
    assert_eq!(sub["project"]["target"][0]["column"], 4);
}

#[test]
fn a_subquery_in_a_join_condition_is_numbered_against_that_join_only() {
    // The condition of a join that is not the last one sees a *shorter* row than the finished FROM
    // clause: `x` and `y` but not `w`. A subquery in it therefore starts at 6, not at 9.
    let v = ok(&same(
        r#"SELECT "x"."a" FROM "t" AS "x"
             JOIN "t" AS "y" ON "y"."a" IN (SELECT "z"."a" FROM "t" AS "z")
             JOIN "t" AS "w" ON TRUE"#,
    ));
    let sub = &find_op(&v, "IN").expect("an IN node")["query"];
    assert_eq!(sub["project"]["target"][0]["column"], 6);
}

#[test]
fn a_join_condition_cannot_reach_a_table_joined_after_it() {
    // Not valid SQL, and the old behaviour was worse than refusing it: resolved against the whole
    // FROM clause it produced an index past the end of the join's row.
    let sql = same(
        r#"SELECT "x"."a" FROM "t" AS "x"
             JOIN "t" AS "y" ON "x"."a" = "z"."a"
             JOIN "t" AS "z" ON TRUE"#,
    );
    match lower_sql(&sql) {
        Err(FrontendError::Schema(m)) => assert!(m.contains("z.a"), "unexpected reason: {m}"),
        other => panic!("expected an unresolved-column refusal, got {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// `lower_with_ddl` — the schema comes from raw Postgres DDL, not from the
// `CREATE TABLE`s in the input.
// ---------------------------------------------------------------------------

/// Raw Postgres, in the shape the corpus actually carries it: unquoted, native type names, a
/// trailing `DEFAULT`, and column order that is *not* alphabetical.
const RAW: &str = "CREATE TABLE t (
    zeta text NOT NULL,
    alpha integer PRIMARY KEY,
    beta double precision DEFAULT 0.0
);";

#[test]
fn the_raw_ddl_wins_over_the_create_tables_in_the_input() {
    // The input declares one column of the wrong type; the DDL declares three. Whichever catalog
    // is in force is visible in the emitted schema, so this cannot pass by accident.
    let src = format!(
        "create table \"t\" (\"alpha\" VARCHAR);\n{q};\n{q};",
        q = "SELECT \"alpha\" FROM \"t\""
    );
    let v = sqleq_frontend::lower_with_ddl(&src, RAW, sqleq_frontend::CatalogSource::Declared)
        .expect("lowers against the raw DDL");
    assert_eq!(v["schemas"].as_array().unwrap().len(), 1);
    assert_eq!(v["schemas"][0]["types"], serde_json::json!(["VARCHAR", "INTEGER", "REAL"]));
}

#[test]
fn not_null_survives_into_the_emitted_schema() {
    // The one thing the Python differential could not check, because the preprocessor has no
    // counterpart to compare against: it discards every `NOT NULL`. `zeta` is declared NOT NULL and
    // `alpha` is a PRIMARY KEY, which implies it; `beta` is neither.
    let src = format!("{q};\n{q};", q = "SELECT \"alpha\" FROM \"t\"");
    let v = sqleq_frontend::lower_with_ddl(&src, RAW, sqleq_frontend::CatalogSource::Declared).unwrap();
    assert_eq!(v["schemas"][0]["nullable"], serde_json::json!([false, false, true]));
    assert_eq!(v["schemas"][0]["key"], serde_json::json!([[1]]));
}

#[test]
fn columns_are_numbered_in_ddl_order_not_alphabetical_order() {
    // The preprocessor re-sorts columns alphabetically, which silently changes what `*` means: it
    // is `SELECT *` compared against an explicit list where the two orders disagree.
    //
    // A bare `SELECT *` lowers to the identity (a bare `scan`), so the assertion that pins the
    // order down is the explicit list. The DDL says zeta, alpha, beta — so *that* list is the
    // identity permutation, and the alphabetical one is not.
    let src = "SELECT *  FROM \"t\";\nSELECT \"zeta\", \"alpha\", \"beta\" FROM \"t\";";
    let v = sqleq_frontend::lower_with_ddl(src, RAW, sqleq_frontend::CatalogSource::Declared).unwrap();
    assert_eq!(v["queries"][0], serde_json::json!({"scan": 0}), "`*` is the identity");
    let cols: Vec<_> = v["queries"][1]["project"]["target"]
        .as_array()
        .expect("a projection")
        .iter()
        .map(|c| c["column"].as_u64().unwrap())
        .collect();
    assert_eq!(cols, vec![0, 1, 2], "DDL order is the identity permutation");

    // Alphabetical order is a different question, and the frontend must not silently answer it.
    let alpha = "SELECT *  FROM \"t\";\nSELECT \"alpha\", \"beta\", \"zeta\" FROM \"t\";";
    let v =
        sqleq_frontend::lower_with_ddl(alpha, RAW, sqleq_frontend::CatalogSource::Declared).unwrap();
    let cols: Vec<_> = v["queries"][1]["project"]["target"]
        .as_array()
        .expect("a projection")
        .iter()
        .map(|c| c["column"].as_u64().unwrap())
        .collect();
    assert_eq!(cols, vec![1, 2, 0], "alphabetical order is a permutation, not the identity");
}

/// `GROUP BY (a, b)` is a row constructor, and grouping on the row is the same partition as
/// grouping on its members — so it lowers to the same IR as the member list, which is also what
/// lets the projection match `a` against a key rather than see a non-grouped column.
#[test]
fn group_by_row_constructor_is_the_member_list() {
    let ddl = "create table t (a INTEGER NOT NULL, b INTEGER NOT NULL);";
    let tuple = "SELECT t.a, t.b FROM t GROUP BY (t.a, t.b)";
    let members = "SELECT t.a, t.b FROM t GROUP BY t.a, t.b";
    let v = ok(&format!("{ddl}\n{tuple};\n{members};"));
    assert_eq!(v["queries"][0], v["queries"][1], "the row constructor lowers to the member list");
}

// --- GROUP BY functional dependence -----------------------------------------------------------
//
// Postgres accepts a non-grouped column when the GROUP BY determines it. The frontend implements
// the one dependence a `CREATE TABLE` proves — group on a NOT NULL key and every other column of
// that table is constant per group — by adding the determined columns to the group keys, which is
// the same partition. Each refusal below is a case where the dependence does *not* hold, and
// grouping on the extra column would split groups the query meant to keep together.

/// The determined column joins the group keys, and an aggregate's argument does not.
#[test]
fn group_by_primary_key_determines_the_other_columns() {
    let ddl = "create table t (id INTEGER PRIMARY KEY, name VARCHAR, amount INTEGER);";
    let q = "SELECT t.id, t.name, sum(t.amount) FROM t GROUP BY t.id";
    let v = ok(&format!("{ddl}\n{q};\n{q};"));
    let g = &v["queries"][0]["group"];
    let keys: Vec<u64> = g["keys"].as_array().unwrap().iter().map(|k| k["column"].as_u64().unwrap()).collect();
    // `name` became a key; `amount` (column 2) stayed the aggregate's argument.
    assert_eq!(keys, vec![0, 1], "the determined column joins the keys");
    assert_eq!(g["function"][0]["operand"][0]["column"], 2, "the summed column is not a key");
}

/// The dependence is read in HAVING as well as in the projection.
#[test]
fn group_by_key_determines_a_column_read_in_having() {
    let ddl = "create table t (id INTEGER PRIMARY KEY, amount INTEGER);\n\
               create table p (tid INTEGER, amount INTEGER);";
    let q = "SELECT t.id FROM t JOIN p ON p.tid = t.id GROUP BY t.id HAVING sum(p.amount) <> t.amount";
    let v = ok(&format!("{ddl}\n{q};\n{q};"));
    // SELECT is narrower than the group output here, so a projection sits above the filter above
    // the group; reach the group by name rather than by that path.
    let g = find_with_field(&v["queries"][0], "group").expect("a group node");
    assert_eq!(g["group"]["keys"].as_array().unwrap().len(), 2, "t.amount, read only in HAVING, became a key");
}

/// A composite key determines only once *every* one of its columns is grouped.
#[test]
fn composite_key_must_be_grouped_whole() {
    let ddl = "create table t (x INTEGER, y INTEGER, b VARCHAR, primary key (x, y));";
    let whole = "SELECT t.x, t.y, t.b FROM t GROUP BY t.x, t.y";
    assert_eq!(ok(&format!("{ddl}\n{whole};\n{whole};"))["queries"][0]["group"]["keys"].as_array().unwrap().len(), 3);
    let half = "SELECT t.x, t.b FROM t GROUP BY t.x";
    refused(&format!("{ddl}\n{half};\n{half};"), "not functionally dependent on GROUP BY");
}

/// A nullable `UNIQUE` is not enough: `GROUP BY u` puts every NULL-keyed row in one group, and on
/// `{(NULL,1),(NULL,2)}` the dependent column is not constant across it.
#[test]
fn nullable_unique_key_does_not_determine() {
    let ddl = "create table t (u INTEGER UNIQUE, b VARCHAR);";
    let q = "SELECT t.u, t.b FROM t GROUP BY t.u";
    refused(&format!("{ddl}\n{q};\n{q};"), "not functionally dependent on GROUP BY");

    // The same key, NOT NULL, does determine — so it is the nullability that refused it above.
    let ddl = "create table t (u INTEGER NOT NULL UNIQUE, b VARCHAR);";
    assert_eq!(ok(&format!("{ddl}\n{q};\n{q};"))["queries"][0]["group"]["keys"].as_array().unwrap().len(), 2);
}

/// The key must be grouped on the *same instance*: under a self-join, `a`'s key says nothing about
/// a column read from `b`.
#[test]
fn key_of_one_alias_does_not_determine_a_column_of_another() {
    let ddl = "create table t (id INTEGER PRIMARY KEY, name VARCHAR);";
    let q = "SELECT a.id, b.name FROM t a JOIN t b ON a.id = b.id GROUP BY a.id";
    refused(&format!("{ddl}\n{q};\n{q};"), "not functionally dependent on GROUP BY");
}

/// A derived table has no declared keys, so nothing it outputs can be shown determined.
#[test]
fn derived_table_column_is_never_determined() {
    let ddl = "create table t (id INTEGER PRIMARY KEY, name VARCHAR);";
    let q = "SELECT d.id, d.name FROM (SELECT id, name FROM t) d GROUP BY d.id";
    refused(&format!("{ddl}\n{q};\n{q};"), "not functionally dependent on GROUP BY");
}

// --- the same dependence, lifted to whole expressions ------------------------------------------
//
// An expression is a function of the columns it reads, so one whose every free column is constant
// within a group is itself constant within it. That is what lets a post-aggregate `EXISTS` or scalar
// subquery — which the post-group scope has no other way to name — join the keys. The free columns
// are read off the *lowered* IR, where `{"column": n}` is the only way to name a value from the
// scope, rather than off the SQL, where a missing expression variant would silently hide one.

const SUB_DDL: &str = "create table t (id INTEGER PRIMARY KEY, nick VARCHAR, amount INTEGER);\n\
                       create table s (v VARCHAR);";

/// The `group` node on the relational spine, reached without descending into expressions. A subquery
/// in the projection carries a `group` of its own, so a depth-first search by field name finds that
/// one first.
fn spine_group(q: &serde_json::Value) -> &serde_json::Value {
    let mut n = q;
    loop {
        if let Some(g) = n.get("group") {
            return g;
        }
        n = ["project", "filter", "distinct"]
            .iter()
            .find_map(|k| n.get(*k))
            .and_then(|x| x.get("source"))
            .expect("no group on the relational spine");
    }
}

/// The group's keys are references into a pre-projection below it, so a key that is a whole
/// expression rather than a column shows up there. Returns `(number of keys, pre-projection)`.
fn group_keys(q: &serde_json::Value) -> (usize, &Vec<serde_json::Value>) {
    let g = spine_group(q);
    let n = g["keys"].as_array().expect("keys").len();
    (n, g["source"]["project"]["target"].as_array().expect("a pre-projection"))
}

/// The post-aggregate `EXISTS` reads only `t.nick`, which the grouped primary key determines, so the
/// whole subquery becomes a group key and the projection resolves to it.
#[test]
fn exists_over_determined_columns_becomes_a_group_key() {
    let q = "SELECT t.id, EXISTS (SELECT 1 FROM s WHERE s.v = t.nick) FROM t GROUP BY t.id";
    let v = ok(&format!("{SUB_DDL}\n{q};\n{q};"));
    let (n, pre) = group_keys(&v["queries"][0]);
    assert_eq!(n, 2, "the EXISTS joined t.id in the key list");
    assert_eq!(pre[1]["operator"], "EXISTS", "the added key is the subquery itself");
}

/// This is what the corpus actually needs: the two `EXISTS` differ (here in a literal), so matching
/// them syntactically fails and only the dependence carries it.
#[test]
fn a_grouped_exists_need_not_match_the_projected_one() {
    let sel = "EXISTS (SELECT 1 FROM s WHERE s.v = t.nick AND s.v = 'a')";
    let grp = "EXISTS (SELECT 1 FROM s WHERE s.v = t.nick AND s.v = 'b')";
    let q = format!("SELECT t.id, {sel} FROM t GROUP BY t.id, {grp}");
    let v = ok(&format!("{SUB_DDL}\n{q};\n{q};"));
    assert_eq!(group_keys(&v["queries"][0]).0, 3, "both subqueries are keys");
}

/// A scalar subquery correlated on the group key itself — no declared key involved, the column is
/// constant because it *is* what the groups are cut by.
#[test]
fn a_subquery_reading_only_the_group_key_is_constant() {
    let q = "SELECT t.nick, (SELECT count(*) FROM s WHERE s.v = t.nick) FROM t GROUP BY t.nick";
    let v = ok(&format!("{SUB_DDL}\n{q};\n{q};"));
    assert_eq!(group_keys(&v["queries"][0]).0, 2);
}

/// One free column that is *not* determined is enough to refuse the whole expression — the group
/// keys here are constants, so nothing is determined and `tags` varies inside the single group.
#[test]
fn a_subquery_reading_an_undetermined_column_is_refused() {
    let q = "SELECT EXISTS (SELECT 1 FROM s WHERE s.v = t.nick), count(*) FROM t GROUP BY 1 + 1";
    refused(&format!("{SUB_DDL}\n{q};\n{q};"), "post-aggregate expression");
}

/// The instance check survives the lift: `a`'s primary key does not license reading `b`'s column,
/// even buried inside a subquery where no syntactic rule would notice it.
#[test]
fn a_subquery_reading_another_alias_of_a_self_join_is_refused() {
    let q = "SELECT a.id, EXISTS (SELECT 1 FROM s WHERE s.v = b.nick) \
             FROM t a JOIN t b ON a.nick = b.nick GROUP BY a.id";
    refused(&format!("{SUB_DDL}\n{q};\n{q};"), "post-aggregate expression");
}

/// The subquery's *own* columns are not free variables of the expression, so they neither need to be
/// determined nor block it — `s.v` here is bound by the subquery's own FROM.
#[test]
fn a_subquerys_own_columns_do_not_have_to_be_determined() {
    let q = "SELECT t.id, EXISTS (SELECT 1 FROM s WHERE s.v > 'a') FROM t GROUP BY t.id";
    let v = ok(&format!("{SUB_DDL}\n{q};\n{q};"));
    assert_eq!(group_keys(&v["queries"][0]).0, 2);
}

/// An aggregate of *this* query still may not become a key: its argument is read per input row, and
/// grouping on it would split the very groups it folds over.
#[test]
fn an_aggregate_is_never_lifted_into_a_key() {
    let q = "SELECT t.id, sum(t.amount) FROM t GROUP BY t.id";
    let v = ok(&format!("{SUB_DDL}\n{q};\n{q};"));
    let (n, pre) = group_keys(&v["queries"][0]);
    assert_eq!(n, 1, "t.id alone; t.amount is determined by it but is the aggregate's argument");
    // The summed column is in the pre-projection *past* the keys, which is where an argument goes.
    assert_eq!(pre.len(), 2);
    assert_eq!(pre[1]["column"], 2, "t.amount, read per row rather than per group");
}

// ---------------------------------------------------------------------------
// Parameter alignment
// ---------------------------------------------------------------------------

/// Assert `sql` is reported misaligned under `InferredSeeded`, with `needle` in the reason.
///
/// `InferredSeeded` because the check lives on the inference path: `Declared` refuses every `$N`
/// before a parameter ever becomes a symbol, so alignment is not a question it can be asked.
fn misaligned(sql: &str, needle: &str) {
    match lower_with(sql, CatalogSource::InferredSeeded) {
        Err(FrontendError::ParameterMisaligned(m)) => {
            assert!(m.contains(needle), "misaligned, but for {m:?} rather than {needle:?}")
        }
        Err(e) => panic!("expected parameter-misaligned mentioning {needle:?}, got {e}"),
        Ok(_) => panic!("expected parameter-misaligned mentioning {needle:?}, but it lowered"),
    }
}

fn aligned(sql: &str) -> serde_json::Value {
    lower_with(sql, CatalogSource::InferredSeeded)
        .unwrap_or_else(|e| panic!("expected Ok, got {e}"))
}

/// Two columns of the *same* type, so that swapping a parameter between them is invisible to
/// inference's own type conflict. Every alignment test below runs against this rather than `T`,
/// because `T`'s columns differ in type and would be caught by the wrong check.
const P: &str = r#"create table "p" ("x" INTEGER, "y" INTEGER, "s" VARCHAR);"#;

fn ppair(q0: &str, q1: &str) -> String {
    format!("{P}\n{q0};\n{q1};")
}

/// An observed pair, reduced: the two sides carry different numbers of placeholders and share the low
/// ones, so `$1` names a different thing on each side. Lowering it hands the prover the diagonal of
/// the space the question ranges over, and that pair's prover verdict is `provable` against a two-row
/// counterexample.
#[test]
fn different_overlapping_parameter_sets_are_misaligned() {
    let sql = ppair(
        r#"SELECT "x" FROM "p" WHERE "x" = $1 AND "y" = $2 AND "x" <> $3"#,
        r#"SELECT "x" FROM "p" WHERE "y" = $1 AND "x" = $2"#,
    );
    misaligned(&sql, "arity");
}

/// A misalignment the frontend's own normalization manufactured is not reported at all.
///
/// An observed pair, reduced. Both sides mention `$1..$4`, so `arity` has nothing to fire on — but
/// `normalize::strip_identical_pagination` deletes the `LIMIT $4` the two sides share, and it is
/// pair-level: it reaches the two outermost queries and not the ones inside a set operation. Query A's
/// only `$4` goes with it while query B keeps two, and the check used to read the stripped trees and see
/// `{1,2,3}` against `{1,2,3,4}`. `params::mentioned` snapshots the sets before any strip runs, which is
/// the fix and the reason it takes a value rather than the queries.
#[test]
fn pagination_the_strip_removed_is_not_a_misalignment() {
    let sql = ppair(
        r#"SELECT "x" FROM "p" WHERE "x" = $1 AND ("y" < $2 OR "y" = $3) LIMIT $4"#,
        r#"(SELECT "x" FROM "p" WHERE "x" = $1 AND "y" < $2 LIMIT $4) UNION ALL (SELECT "x" FROM "p" WHERE "x" = $1 AND "y" = $3 LIMIT $4) LIMIT $4"#,
    );
    // Whether this shape *lowers* is a separate question — the inner `LIMIT`s are a `Sort` under a set
    // operation — so the assertion is about the one refusal that would be wrong, not about success.
    if let Err(FrontendError::ParameterMisaligned(m)) = lower_with(&sql, CatalogSource::InferredSeeded) {
        panic!("both sides mention $1..$4; the strip manufactured this: {m}");
    }
}

/// When the two roles have *different* types, inference gets there first: one `Uf` is shared across
/// the pair, so a parameter unified with an INTEGER column on one side and a VARCHAR column on the
/// other is a type conflict before alignment is ever asked. A second, incidental detector — pinned
/// here because it is the reason the alignment tests need same-typed columns, and because it is why
/// the residual hole is specifically the *same*-type permutation.
#[test]
fn a_permutation_across_types_is_caught_by_inference_instead() {
    let sql = pair(
        r#"SELECT "a" FROM "t" WHERE "a" = $1 AND "b" = $2"#,
        r#"SELECT "a" FROM "t" WHERE "a" = $2 AND "b" = $1"#,
    );
    match lower_with(&sql, CatalogSource::InferredSeeded) {
        Err(FrontendError::Schema(m)) => assert!(m.contains("type conflict"), "{m}"),
        other => panic!("expected a type conflict, got {other:?}"),
    }
}

/// A misalignment that *manufactured* a type conflict is reported as the misalignment.
///
/// An observed pair, reduced: query B numbers a placeholder the rewrite added, so its `$2` is a VARCHAR
/// column where query A's `$2` is an INTEGER one. Identifying them by index is what puts two unrelated
/// values in one type class, and before this rule the row reported `type conflict VARCHAR/INTEGER` — a
/// true statement about a class the frontend itself created, pointing at a column that is not wrong.
/// The pair is refused either way; what this pins is which fact the caller is handed.
#[test]
fn a_conflict_the_misalignment_created_is_reported_as_the_misalignment() {
    let sql = ppair(
        r#"SELECT "x" FROM "p" WHERE "x" = $1 AND "y" = $2"#,
        r#"SELECT "x" FROM "p" WHERE "x" = $1 AND "s" = $2 AND "y" = $3"#,
    );
    misaligned(&sql, "arity");
}

/// The other half of the precedence rule: a conflict the misalignment could not have caused is still
/// reported as the conflict, even though `arity` also fires.
///
/// `"x" = "s"` disagrees inside query A alone — INTEGER against VARCHAR, both declared — so pulling the
/// two queries' parameters apart leaves it exactly where it was. A real pair has this shape and
/// keeps its `type conflict`, which is what stops the promotion from swallowing every conflict that
/// happens to sit next to a renumbering.
#[test]
fn a_conflict_the_misalignment_could_not_have_created_is_still_the_conflict() {
    let sql = ppair(
        r#"SELECT "x" FROM "p" WHERE "x" = "s" AND "x" = $1 AND "y" = $2"#,
        r#"SELECT "x" FROM "p" WHERE "x" = $1"#,
    );
    match lower_with(&sql, CatalogSource::InferredSeeded) {
        Err(FrontendError::Schema(m)) => assert!(m.contains("type conflict"), "{m}"),
        other => panic!("expected a type conflict, got {other:?}"),
    }
}

/// The same rule at the *lowering* gate, on the refusal that found the whole issue.
///
/// An observed pair's shape: `is_deleted = $4 LIMIT $5` against `is_deleted = false LIMIT $4`, here with
/// a VARCHAR column standing in for the boolean. Index binding puts a filter value and a row count in
/// one type class, the count comes out VARCHAR, and `lower.rs`'s count guard refuses it — a count that
/// is the wrong type only because the two queries number their placeholders differently.
#[test]
fn a_count_the_misalignment_mistyped_is_reported_as_the_misalignment() {
    let sql = ppair(
        r#"SELECT "x" FROM "p" WHERE "x" = $1 AND "s" = $2 LIMIT $3"#,
        r#"SELECT "x" FROM "p" WHERE "x" = $1 LIMIT $2"#,
    );
    misaligned(&sql, "arity");
}

/// The control for the test above: the count guard is still there. Equal parameter sets, so nothing is
/// misaligned and nothing is promoted — the same mistyped count is reported as the count.
#[test]
fn a_mistyped_count_with_nothing_misaligned_is_still_the_count() {
    let sql = ppair(
        r#"SELECT "x" FROM "p" WHERE "s" = $1 LIMIT $1"#,
        r#"SELECT "x" FROM "p" WHERE "s" = $1 LIMIT 1"#,
    );
    match lower_with(&sql, CatalogSource::InferredSeeded) {
        Err(FrontendError::Unsupported(m)) => assert!(m.contains("count typed"), "{m}"),
        other => panic!("expected the count guard, got {other:?}"),
    }
}

/// Disjoint parameter sets are *not* misaligned: with no index on both sides, index binding asks the
/// two queries about independent values, which is stronger than any correspondence the caller could
/// have meant. Refusing here would cost the corpus's pagination rewrites for no soundness.
#[test]
fn disjoint_parameter_sets_are_not_misaligned() {
    let sql = pair(
        r#"SELECT "a" FROM "t" WHERE "a" = $1"#,
        r#"SELECT "a" FROM "t" WHERE "a" = $2"#,
    );
    let v = aligned(&sql);
    // Distinct symbols, which is why the pair is sound-but-unprovable rather than refused.
    assert!(v.to_string().contains("QP1") && v.to_string().contains("QP2"));
}

/// A permutation the sets cannot show: same indices on both sides, different roles. Caught by the
/// columns each parameter is compared against, read off inference's attribution rather than the text.
#[test]
fn a_permutation_of_equal_parameter_sets_is_misaligned() {
    let sql = ppair(
        r#"SELECT "x" FROM "p" WHERE "x" = $1 AND "y" = $2"#,
        r#"SELECT "x" FROM "p" WHERE "x" = $2 AND "y" = $1"#,
    );
    misaligned(&sql, "order");
}

/// The false positive the `order` check must not have: reordering an `IN` list is a no-op, and both
/// parameters are compared against the same column on both sides, so the role sets coincide.
#[test]
fn reordering_an_in_list_is_not_a_permutation_of_roles() {
    let sql = pair(
        r#"SELECT "a" FROM "t" WHERE "a" IN ($1, $2)"#,
        r#"SELECT "a" FROM "t" WHERE "a" IN ($2, $1)"#,
    );
    aligned(&sql);
}

/// `x = ANY(ARRAY[...])` reaches the elements, so the array spelling of the same predicate is also a
/// role match rather than a permutation.
#[test]
fn the_array_spelling_of_a_list_attributes_each_element() {
    let sql = pair(
        r#"SELECT "a" FROM "t" WHERE "a" IN ($1, $2)"#,
        r#"SELECT "a" FROM "t" WHERE "a" = ANY(ARRAY[$2, $1])"#,
    );
    aligned(&sql);
}

/// A parameter with no column to be compared against yields no evidence, and no evidence never
/// fires: `$1` is a projected value on one side and a count on the other, which the role walk cannot
/// see and must not guess about.
#[test]
fn a_parameter_with_no_comparison_yields_no_role_evidence() {
    let sql = pair(
        r#"SELECT $1 FROM "t" WHERE "a" = $2"#,
        r#"SELECT $1 FROM "t" WHERE "a" = $2"#,
    );
    aligned(&sql);
}

/// A cast over the compared column costs the slot its role evidence, and no evidence never fires —
/// so this genuine `$1`/`$2` swap lowers. That is a **completeness** limitation of `role_of`, which
/// resolves only a bare `Identifier`/`CompoundIdentifier` through [`Inferred::col`]; every other
/// operand shape (a call, an arithmetic expression, a cast) yields `None` by construction.
///
/// It is deliberately pinned rather than fixed, and pinned here rather than left implicit, because
/// the error runs in the *unsound* direction: a suppressed `order` is a missed misalignment, not a
/// lost proof. What bounds it is measurement, not the shape — a corpus scan found every slot blinded
/// this way, looked through the cast at each one, and found **none of them actually misaligned**.
/// Fixing it means unwrapping casts in `role_of`, which would flip this
/// assertion to `misaligned(&sql, "order")`; the target is unqualified so rule 4 does not delete it.
#[test]
fn a_cast_over_the_compared_column_costs_the_role_evidence() {
    let sql = ppair(
        r#"SELECT "x" FROM "p" WHERE "x"::varchar(8) = $1 AND "y"::varchar(8) = $2"#,
        r#"SELECT "x" FROM "p" WHERE "y"::varchar(8) = $1 AND "x"::varchar(8) = $2"#,
    );
    aligned(&sql);
}

/// The discriminator for the test above: it is the cast *node*, not any stage that ran before the
/// check. `x::integer` on an `INTEGER` column is rule 4's no-op, deleted outright, so the bare
/// identifier is back by the time `check_roles` looks — and the same swap fires. Parentheses behave
/// the same way, since `unwrap_nested` sees through them. So reading the post-`rewrite_casts` copy is
/// not the cause: it is what *restores* the evidence here.
#[test]
fn a_cast_the_rewrite_deletes_keeps_the_role_evidence() {
    misaligned(
        &ppair(
            r#"SELECT "x" FROM "p" WHERE "x"::integer = $1 AND "y"::integer = $2"#,
            r#"SELECT "x" FROM "p" WHERE "y"::integer = $1 AND "x"::integer = $2"#,
        ),
        "order",
    );
    misaligned(
        &ppair(
            r#"SELECT "x" FROM "p" WHERE ("x") = $1 AND ("y") = $2"#,
            r#"SELECT "x" FROM "p" WHERE ("y") = $1 AND ("x") = $2"#,
        ),
        "order",
    );
}

/// The misalignment is raised *after* lowering, so a pair that is also unsupported reports the
/// construct. That ordering is what makes the `parameter-misaligned` bucket a count of rows nothing
/// else refused, rather than a reshuffle of rows already refused for a construct.
///
/// Also the yielding half of the lowering gate's precedence rule: no renumbering invents or removes a
/// construct, so the counterfactual refuses too and the window function is what the row reports. This
/// is the common case by a wide margin — nearly every row that reaches the gate with a verdict
/// pending yields, and every one of them to a construct or an unresolved name.
#[test]
fn a_construct_refusal_outranks_a_misalignment() {
    let sql = pair(
        r#"SELECT COUNT(*) OVER () FROM "t" WHERE "a" = $1 AND "b" = $2"#,
        r#"SELECT "a" FROM "t" WHERE "a" = $1"#,
    );
    match lower_with(&sql, CatalogSource::InferredSeeded) {
        Err(FrontendError::Unsupported(m)) => assert!(m.contains("window function"), "{m}"),
        other => panic!("expected the window refusal to win, got {other:?}"),
    }
}

// ---------------------------------------------------------------------------------------------
// DISTINCT ON
//
// The encoding is `Project(trim) <- Group{uninterpreted aggregates} <- Project(outputs ++ keys)`.
// What these tests protect is the *identity* of the opaque operator: everything that distinguishes
// one DISTINCT ON from another has to end up either in the extended projection (as a value) or in
// the operator's name (as a position and a direction). A gap in either place lets two different
// operators land on one term, which is a false proof rather than a missed one.

/// Every `DISTINCT_ON#…` operator name anywhere in `v`.
fn distinct_on_ops(v: &serde_json::Value) -> Vec<String> {
    collect_field(v, "operator")
        .into_iter()
        .filter_map(|o| o.as_str().map(str::to_owned))
        .filter(|o| o.starts_with("DISTINCT_ON#"))
        .collect()
}

const EV: &str = r#"create table "ev" ("k" INTEGER, "t" INTEGER, "v" VARCHAR);"#;

fn ev_pair(q0: &str, q1: &str) -> serde_json::Value {
    ok(&format!("{EV}\n{q0};\n{q1};"))
}

#[test]
fn distinct_on_lowers_to_a_group_of_uninterpreted_aggregates() {
    let q = r#"SELECT DISTINCT ON ("k") "k", "v" FROM "ev" ORDER BY "k", "t" DESC"#;
    let v = ev_pair(q, q);
    let g = find_with_field(&v["queries"][0], "group").expect("a group node")["group"].clone();
    // One key, and one aggregate per output column.
    assert_eq!(g["keys"].as_array().unwrap().len(), 1);
    let funcs = g["function"].as_array().unwrap();
    assert_eq!(funcs.len(), 2);
    for f in funcs {
        // `ignoreNulls` defaults to true in the prover and would drop candidate rows with a NULL.
        assert_eq!(f["ignoreNulls"], false, "DISTINCT ON must not ignore nulls");
        // Under two arguments the prover emits an *interpreted* Aggr instead of the opaque HOp.
        assert!(f["operand"].as_array().unwrap().len() >= 2, "needs >= 2 args to stay opaque");
    }
    // `Group` yields `keys ++ columns`; the top projection trims the key back off.
    assert_eq!(v["queries"][0]["project"]["target"].as_array().unwrap().len(), 2);
}

/// The ordering column is not an output column, so only the extended projection can carry it. If it
/// does not, the two sides below — largest `t` per key against smallest — become the same term.
#[test]
fn the_ordering_value_is_an_argument_not_just_a_name() {
    let v = ev_pair(
        r#"SELECT DISTINCT ON ("k") "k", "v" FROM "ev" ORDER BY "k", "t" DESC"#,
        r#"SELECT DISTINCT ON ("k") "k", "v" FROM (SELECT "k", "v", -"t" AS "t" FROM "ev") AS "s" ORDER BY "k", "t" DESC"#,
    );
    assert_ne!(v["queries"][0], v["queries"][1], "-t must not lower like t");
}

#[test]
fn opposite_sort_directions_are_different_operators() {
    let v = ev_pair(
        r#"SELECT DISTINCT ON ("k") "k", "v" FROM "ev" ORDER BY "k", "t" ASC"#,
        r#"SELECT DISTINCT ON ("k") "k", "v" FROM "ev" ORDER BY "k", "t" DESC"#,
    );
    let (a, b) = (distinct_on_ops(&v["queries"][0]), distinct_on_ops(&v["queries"][1]));
    assert_eq!(a.len(), 2);
    assert_ne!(a, b, "ASC and DESC must not mint the same symbol");
}

#[test]
fn null_placement_is_part_of_the_operator() {
    let v = ev_pair(
        r#"SELECT DISTINCT ON ("k") "k", "v" FROM "ev" ORDER BY "k", "t" NULLS FIRST"#,
        r#"SELECT DISTINCT ON ("k") "k", "v" FROM "ev" ORDER BY "k", "t" NULLS LAST"#,
    );
    assert_ne!(distinct_on_ops(&v["queries"][0]), distinct_on_ops(&v["queries"][1]));
}

#[test]
fn key_priority_is_part_of_the_operator() {
    let v = ev_pair(
        r#"SELECT DISTINCT ON ("k") "k", "t", "v" FROM "ev" ORDER BY "k", "t", "v""#,
        r#"SELECT DISTINCT ON ("k") "k", "t", "v" FROM "ev" ORDER BY "k", "v", "t""#,
    );
    assert_ne!(distinct_on_ops(&v["queries"][0]), distinct_on_ops(&v["queries"][1]));
}

/// The mirror of the tests above: spellings of *one* ordering must converge, or the encoding buys
/// soundness by refusing to prove anything.
#[test]
fn the_order_defaults_are_resolved_in_the_operator_too() {
    let v = ev_pair(
        r#"SELECT DISTINCT ON ("k") "k", "t" FROM "ev" ORDER BY "k", "t""#,
        r#"SELECT DISTINCT ON ("k") "k", "t" FROM "ev" ORDER BY 1, 2 ASC NULLS LAST"#,
    );
    assert_eq!(v["queries"][0], v["queries"][1], "one ordering, two spellings, one term");
}

/// Postgres resolves a bare `ORDER BY` key against the select list before the FROM scope; a
/// qualified one is always an input column. Getting that backwards orders by the wrong value.
#[test]
fn an_order_key_prefers_the_output_alias() {
    let sql = r#"create table "t2" ("k" INTEGER, "a" INTEGER, "b" INTEGER);
SELECT DISTINCT ON ("k") "k", "b" AS "a" FROM "t2" ORDER BY "k", "a";
SELECT DISTINCT ON ("k") "k", "b" AS "a" FROM "t2" ORDER BY "k", "t2"."a";"#;
    let v = ok(sql);
    assert_ne!(v["queries"][0], v["queries"][1], "`a` is the alias for b, `t2.a` is the column");
}

/// A key the projection drops is appended to the extended projection rather than refused.
#[test]
fn distinct_on_may_key_on_a_column_it_does_not_project() {
    let v = ev_pair(
        r#"SELECT DISTINCT ON ("k") "v" FROM "ev" WHERE "t" > 0 ORDER BY "k", "t" DESC"#,
        r#"SELECT DISTINCT ON ("k") "v" FROM "ev" WHERE 0 < "t" ORDER BY "k", "t" DESC"#,
    );
    // One output column, but the tuple the aggregate ranges over also holds `k` and `t`.
    assert_eq!(v["queries"][0]["project"]["target"].as_array().unwrap().len(), 1);
    let g = find_with_field(&v["queries"][0], "group").expect("a group node")["group"].clone();
    assert_eq!(g["function"][0]["operand"].as_array().unwrap().len(), 3);
}

/// A one-column tuple would take the prover's *interpreted* single-argument branch, so it is padded.
#[test]
fn a_one_column_distinct_on_is_padded_to_stay_opaque() {
    let q = r#"SELECT DISTINCT ON ("k") "k" FROM "ev""#;
    let v = ev_pair(q, q);
    let g = find_with_field(&v["queries"][0], "group").expect("a group node")["group"].clone();
    let args = g["function"][0]["operand"].as_array().unwrap();
    assert_eq!(args.len(), 2);
    assert_eq!(args[0], args[1]);
}

/// A `SELECT` reached through a set operation is governed by an `ORDER BY` that is not its own, and
/// reading that as "no ORDER BY" would give two differently-ordered operators the same symbol.
#[test]
fn refuses_distinct_on_under_a_set_operation() {
    refused(
        &format!(
            "{EV}\nSELECT DISTINCT ON (\"k\") \"k\" FROM \"ev\" UNION SELECT \"k\" FROM \"ev\" ORDER BY \"k\";\n\
             SELECT \"k\" FROM \"ev\";"
        ),
        "DISTINCT ON under a set operation",
    );
}

/// The keys would resolve against the post-aggregation output, which this path does not model.
#[test]
fn refuses_distinct_on_over_an_aggregate() {
    refused(
        &format!(
            "{EV}\nSELECT DISTINCT ON (\"k\") \"k\", COUNT(*) FROM \"ev\" GROUP BY \"k\";\n\
             SELECT \"k\" FROM \"ev\";"
        ),
        "DISTINCT ON over an aggregate query",
    );
}

// --- Postgres system columns ------------------------------------------------------------------
//
// Postgres puts `ctid`, `xmin` and friends on every table and no DDL declares them. The catalog
// appends the ones a pair actually names (`catalog::add_system_columns`), which splits what a
// binding's `cols` had meant into two: the *width* the prover indexes into, and the narrower set
// `*` expands to. Every test below pins one side of that split.

/// The `"column"` level of each projection target, in projection order (which
/// [`collect_field`] does not preserve).
fn targets(q: &serde_json::Value) -> Vec<u64> {
    q["project"]["target"]
        .as_array()
        .expect("a projection")
        .iter()
        .map(|t| t["column"].as_u64().expect("a column reference"))
        .collect()
}

/// Every `"column"` level anywhere in `v`, sorted — for the tests that pin *which* levels a
/// condition reaches rather than the order it reaches them in.
fn levels(v: &serde_json::Value) -> Vec<u64> {
    let mut out: Vec<u64> =
        collect_field(v, "column").iter().filter_map(|c| c.as_u64()).collect();
    out.sort_unstable();
    out
}

/// The reference the whole change exists for: `t.ctid` resolves instead of refusing, and it lands
/// past the declared columns as the opaque sort that supports equality and nothing else.
#[test]
fn a_qualified_system_column_resolves() {
    let v = ok(&same(r#"SELECT "t"."ctid" FROM "t""#));
    assert_eq!(v["schemas"][0]["types"], serde_json::json!(["INTEGER", "VARCHAR", "VARBINARY", "VARBINARY"]));
    assert_eq!(v["queries"][0]["project"]["target"], serde_json::json!([{ "column": 3, "type": "VARBINARY" }]));
    assert_eq!(targets(&v["queries"][0]), vec![3]);
}

/// Unqualified too — nothing about the reference has to name the table.
#[test]
fn a_bare_system_column_resolves() {
    let v = ok(&same(r#"SELECT "a" FROM "t" WHERE "ctid" = "ctid""#));
    assert_eq!(levels(&v["queries"][0]), vec![0, 3, 3]);
}

/// `xmin` as well as `ctid`, and through `lower_with_ddl` — the entry point for a caller holding
/// raw DDL, which reaches `add_system_columns` by a different path from the `lower_sql` every test
/// above goes through. Both paths must append the same columns, or a pair lowers on one and
/// refuses on the other against the very same schema.
#[test]
fn a_system_column_resolves_through_raw_ddl() {
    let q = r#"SELECT "t"."ctid", "t"."xmin", "a" FROM "t""#;
    let src = format!("{q};\n{q};");
    lower_with_ddl(&src, T, CatalogSource::Declared)
        .expect("ctid and xmin are columns of every table, declared or not");
}

/// The negative half, and the reason the list is not simply "whatever Postgres exposes": since
/// PG12 `oid` is on system catalogs alone, so it is *not* universal. Resolving it against a user
/// table would invent a column the table does not have — the one thing worse than refusing.
#[test]
fn oid_is_not_a_system_column() {
    match lower_sql(&same(r#"SELECT "t"."oid" FROM "t""#)) {
        Err(FrontendError::Schema(m)) => assert!(m.contains("t.oid"), "{m}"),
        other => panic!("expected t.oid to be unresolvable, got {other:?}"),
    }
}

/// The test that matters. `SELECT *` is three columns in Postgres whether or not the query reads
/// `ctid` elsewhere, so the shortcut that lowers a pure wildcard to the bare scan — which *would*
/// yield four — has to stand down here and emit the projection instead.
#[test]
fn a_wildcard_does_not_expand_a_system_column() {
    let v = ok(&same(r#"SELECT * FROM "t" WHERE "t"."ctid" = "t"."ctid""#));
    // The schema still has four: the prover has to see the column or level 3 is out of range.
    assert_eq!(v["schemas"][0]["types"].as_array().unwrap().len(), 4);
    assert_eq!(targets(&v["queries"][0]), vec![0, 1, 2], "a projection, not the bare scan");
}

/// `t.*` names one binding rather than all of them, and is bounded by the same prefix.
#[test]
fn a_qualified_wildcard_does_not_expand_a_system_column() {
    let v = ok(&same(r#"SELECT "t".* FROM "t" WHERE "t"."ctid" = "t"."ctid""#));
    assert_eq!(targets(&v["queries"][0]), vec![0, 1, 2]);
}

/// A derived table exposes the shape it projects. A system column of the table underneath is not
/// part of that shape, so reaching for it through the alias is an unknown column — which is what
/// Postgres says too.
#[test]
fn a_derived_table_does_not_expose_a_system_column() {
    let sql = same(r#"SELECT "d"."ctid" FROM (SELECT * FROM "t" WHERE "t"."ctid" = "t"."ctid") "d""#);
    match lower_sql(&sql) {
        Err(FrontendError::Schema(m)) => assert!(m.contains("d.ctid"), "{m}"),
        other => panic!("expected d.ctid to be unresolvable, got {other:?}"),
    }
}

/// The width side of the split. The outer row is four columns wide once `ctid` is on it, so the
/// correlated subquery's own bindings start at 4 — get this wrong and every index in the subquery
/// silently reaches an enclosing column instead.
#[test]
fn a_correlated_system_column_reference_keeps_the_levels_apart() {
    let v = ok(&same(
        r#"SELECT "a" FROM "t" WHERE EXISTS (SELECT 1 FROM "t" "u" WHERE "u"."ctid" = "t"."ctid")"#,
    ));
    let cols = levels(&v["queries"][0]);
    assert!(cols.contains(&7), "u.ctid should be level 7, got {cols:?}");
    assert!(cols.contains(&3), "t.ctid should be level 3, got {cols:?}");
}

/// The containment property, as a test: a pair that names no system column gets the catalog it
/// always got. This is what makes a corpus-wide diff a check of the rows that name a system column
/// rather than of every row in the file.
#[test]
fn a_pair_naming_no_system_column_is_untouched() {
    let v = ok(&same(r#"SELECT "a", "b" FROM "t""#));
    assert_eq!(v["schemas"][0]["types"], serde_json::json!(["INTEGER", "VARCHAR", "VARBINARY"]));
    assert_eq!(v["schemas"][0]["nullable"].as_array().unwrap().len(), 3);
}

/// `ctid` is unique and non-null in Postgres and we assert neither, because both directions
/// *shrink* the instances the prover quantifies over — the argument `build_inferred` makes about
/// inferred nullability, applied to a column we invented outright.
#[test]
fn an_appended_system_column_carries_no_key_and_no_not_null() {
    let v = ok(&same(r#"SELECT "t"."ctid" FROM "t""#));
    assert_eq!(v["schemas"][0]["nullable"], serde_json::json!([true, true, true, true]));
    // The declared UNIQUE(a) is still there, and still index 0 — appending at the end is what keeps
    // the existing key indices valid.
    assert_eq!(v["schemas"][0]["key"], serde_json::json!([[0]]));
}

// --- GROUP BY naming a select-list alias --------------------------------------------------------
//
// Postgres lets a bare, unqualified `GROUP BY` name refer to an output column, with input columns
// taking precedence. `lower_group_key` resolves the alias out of the projection, since `out_cols`
// do not exist yet when the keys are lowered. Each refusal below is a case where using the alias
// would be answering a resolution question Postgres answers differently, or not at all.

/// The alias resolves, and the grouped relation is the one `GROUP BY <that expression>` gives.
#[test]
fn group_by_resolves_a_select_list_alias() {
    let ddl = "create table e (event_id INTEGER, kind VARCHAR);";
    let aliased = "SELECT e.event_id AS eid, count(*) FROM e GROUP BY eid";
    let spelled = "SELECT e.event_id AS eid, count(*) FROM e GROUP BY e.event_id";
    let v = ok(&format!("{ddl}\n{aliased};\n{spelled};"));
    // The point of the fix: the two sides lower to the *same* IR, so the pair is provable.
    assert_eq!(v["queries"][0], v["queries"][1], "the alias and the expression it names agree");
    assert!(find_with_field(&v["queries"][0], "group").is_some());
}

/// The alias may name an arbitrary expression, not just a column — that is where it earns its keep,
/// since the pair's other side typically spells the expression out.
#[test]
fn group_by_alias_over_an_expression() {
    let ddl = "create table m (ts INTEGER, v INTEGER);";
    let aliased = "SELECT m.ts + 1 AS hour, sum(m.v) FROM m GROUP BY hour";
    let spelled = "SELECT m.ts + 1 AS hour, sum(m.v) FROM m GROUP BY m.ts + 1";
    let v = ok(&format!("{ddl}\n{aliased};\n{spelled};"));
    assert_eq!(v["queries"][0], v["queries"][1]);
}

/// An input column of the same name **wins**, which is Postgres's rule and not a tie-break we get
/// to choose: `GROUP BY a` here groups on `t2.a`, not on the aliased `t2.b`. Note this is the
/// *opposite* of the `ORDER BY` rule one function above — there an output name wins — and the two
/// asymmetric rules are both Postgres's.
#[test]
fn an_input_column_beats_a_select_list_alias() {
    // `a` is the key, so grouping on it determines `b` and the projection lowers either way; that
    // is what lets the test be about which column was chosen rather than about a refusal.
    let ddl = r#"create table t2 ("a" INTEGER PRIMARY KEY, "b" INTEGER);"#;
    let shadowed = r#"SELECT "t2"."b" AS "a", count(*) FROM "t2" GROUP BY "a""#;
    let by_input = r#"SELECT "t2"."b" AS "a", count(*) FROM "t2" GROUP BY "t2"."a""#;
    let by_alias = r#"SELECT "t2"."b" AS "a", count(*) FROM "t2" GROUP BY "t2"."b""#;
    let v = ok(&format!("{ddl}\n{shadowed};\n{by_input};"));
    assert_eq!(v["queries"][0], v["queries"][1], "the bare name is the input column");
    // The other half of the claim: it is not the alias. Without this the assertion above would
    // also pass if the two spellings collapsed for some unrelated reason.
    let w = ok(&format!("{ddl}\n{shadowed};\n{by_alias};"));
    assert_ne!(w["queries"][0], w["queries"][1], "the bare name is not the aliased expression");
}

/// Two items sharing the alias is a resolution question with no right answer, declined exactly as
/// `order_key_index` declines it for `ORDER BY`.
#[test]
fn group_by_declines_a_duplicated_alias() {
    let ddl = "create table d (x INTEGER, y INTEGER);";
    let q = "SELECT d.x AS k, d.y AS k, count(*) FROM d GROUP BY k";
    match lower_sql(&format!("{ddl}\n{q};\n{q};")) {
        Err(FrontendError::Schema(m)) => assert!(m.contains("ambiguous GROUP BY key k"), "{m}"),
        other => panic!("expected an ambiguity refusal, got {other:?}"),
    }
}

/// Postgres rejects an aggregate in `GROUP BY`, so resolving the alias to one would be lowering a
/// query that does not run.
#[test]
fn group_by_declines_an_alias_over_an_aggregate() {
    let ddl = "create table g (x INTEGER, v INTEGER);";
    let q = "SELECT g.x, sum(g.v) AS s FROM g GROUP BY s";
    match lower_sql(&format!("{ddl}\n{q};\n{q};")) {
        Err(FrontendError::Schema(m)) => assert!(m.contains("aggregate in GROUP BY key s"), "{m}"),
        other => panic!("expected an aggregate refusal, got {other:?}"),
    }
}

/// A qualified name never denotes an output column, so the fallback is not taken and the original
/// error is what surfaces.
#[test]
fn a_qualified_group_by_name_does_not_reach_the_alias() {
    let ddl = "create table q (x INTEGER);";
    let sql = "SELECT q.x AS eid, count(*) FROM q GROUP BY q.eid";
    match lower_sql(&format!("{ddl}\n{sql};\n{sql};")) {
        Err(FrontendError::Schema(m)) => assert!(m.contains("unresolved column"), "{m}"),
        other => panic!("expected the original unresolved-column error, got {other:?}"),
    }
}

/// When no item carries the name, the *original* error is reported — the fallback must not move a
/// row's blocker onto a construct that was never the cause.
#[test]
fn an_unmatched_group_by_name_keeps_its_original_error() {
    let ddl = "create table u (x INTEGER);";
    let sql = "SELECT u.x AS eid, count(*) FROM u GROUP BY nosuch";
    match lower_sql(&format!("{ddl}\n{sql};\n{sql};")) {
        Err(FrontendError::Schema(m)) => assert!(m.contains("unresolved column nosuch"), "{m}"),
        other => panic!("expected unresolved column nosuch, got {other:?}"),
    }
}

/// `GROUP BY 1` is a select-list *position* in Postgres, and nothing here resolves it. Lowering it
/// as the literal 1 would put every row in one group and answer a different query, so it is refused
/// rather than left to do that quietly.
#[test]
fn group_by_position_is_refused() {
    let ddl = "create table p2 (x INTEGER, v INTEGER);";
    let q = "SELECT p2.x, sum(p2.v) FROM p2 GROUP BY 1";
    refused(&format!("{ddl}\n{q};\n{q};"), "GROUP BY position 1");
}

/// The determined-column extension reads the key list, so it has to see the value the alias
/// resolved to — a key reached through an alias is a key like any other.
#[test]
fn a_key_reached_through_an_alias_still_determines_the_other_columns() {
    let ddl = "create table k (id INTEGER PRIMARY KEY, name VARCHAR, amount INTEGER);";
    let q = "SELECT k.id AS pk, k.name, sum(k.amount) FROM k GROUP BY pk";
    let v = ok(&format!("{ddl}\n{q};\n{q};"));
    let g = find_with_field(&v["queries"][0], "group").expect("a group node");
    let keys: Vec<u64> = g["group"]["keys"]
        .as_array()
        .unwrap()
        .iter()
        .map(|x| x["column"].as_u64().unwrap())
        .collect();
    assert_eq!(keys, vec![0, 1], "`name` joined the keys through the aliased key");
}

// --- casts over array literals ------------------------------------------------------------------

/// The three tests below run under `InferredSeeded` rather than through [`ok`]/[`refused`], for the
/// reason spelled out on `a_parameter_is_a_count`: `Declared` skips the whole inference block, and
/// with it `casts::substitute_params`, so it refuses every `$N` on sight — including in shapes this
/// rewrite does not touch. `InferredSeeded` is what the harness runs (`--catalog infer-seeded`), and
/// it is also the only mode in which `casts::rewrite_casts` — the refusal being displaced here — runs
/// at all.
fn ok_inf(sql: &str) -> serde_json::Value {
    lower_with(sql, CatalogSource::InferredSeeded)
        .unwrap_or_else(|e| panic!("expected Ok, got {e}"))
}

fn refused_inf(sql: &str, needle: &str) {
    match lower_with(sql, CatalogSource::InferredSeeded) {
        Err(FrontendError::Unsupported(m)) => {
            assert!(m.contains(needle), "refused, but for {m:?} rather than {needle:?}")
        }
        Err(e) => panic!("expected an Unsupported refusal mentioning {needle:?}, got {e}"),
        Ok(_) => panic!("expected an Unsupported refusal mentioning {needle:?}, but it lowered"),
    }
}

/// End to end, and the point of the normalization: `casts` still refuses a cast over an array, but
/// `normalize::distribute_array_casts` has removed the shape by the time it looks, so the pair
/// lowers instead. `lower_quantified` then gives the literal its exact OR-expansion, which is what
/// the refusal existed to protect.
#[test]
fn a_cast_over_an_array_literal_under_any_lowers() {
    let q = r#"SELECT "a" FROM "t" WHERE "a" = ANY(ARRAY[$1, $2]::bigint[])"#;
    let v = ok_inf(&same(q));
    assert_eq!(v["queries"][0], v["queries"][1]);
    // The expansion, not an opaque call: two equalities joined by OR.
    let s = v["queries"][0].to_string();
    assert!(s.contains("\"OR\""), "expected the OR-expansion, got {s}");
    // Both parameters are reached, each as its own symbol.
    assert!(s.contains("QP1") && s.contains("QP2"), "expected both parameters, got {s}");
}

/// The element cast is what types the placeholder, which is the reason this runs before inference
/// rather than inside `casts`. `$1` has no other evidence in the pair, so without the cast it would
/// reach lowering as `Ty::Opaque` and mismatch the `VARCHAR` column it is compared against.
#[test]
fn the_element_cast_types_the_placeholder() {
    let q = r#"SELECT "b" FROM "t" WHERE "b" = ANY(ARRAY[$1]::varchar[])"#;
    let v = ok_inf(&same(q));
    assert_eq!(v["queries"][0], v["queries"][1]);
    let s = v["queries"][0].to_string();
    assert!(s.contains("QP1"), "expected the parameter, got {s}");
}

/// The refusal is still there for the shapes the rewrite deliberately leaves alone: an empty literal
/// has no elements to push the cast onto, and a `Tuple` operand never was an array.
#[test]
fn a_cast_over_an_array_is_still_refused_where_it_is_not_distributed() {
    refused_inf(&same(r#"SELECT COALESCE("b", ARRAY[]::varchar[]) FROM "t""#), "Array");
    refused_inf(&same(r#"SELECT ("a", "b")::record FROM "t""#), "Tuple");
}

// ---------------------------------------------------------------------------
// Parameter alignment: the `shape` sub-reason
// ---------------------------------------------------------------------------

/// A table whose columns are all the same type, so an `INSERT` pair over it has no type conflict of
/// its own to confuse the shape reading with.
const V: &str = r#"create table "v" ("a" VARCHAR, "b" VARCHAR);"#;

fn vpair(q0: &str, q1: &str) -> String {
    format!("{V}\n{q0};\n{q1};")
}

/// Assert `sql` reaches *some* outcome that is not a `shape` misalignment. The control for every
/// test below: what it refuses with is beside the point, only that this check kept quiet.
fn not_misshapen(sql: &str) {
    if let Err(FrontendError::ParameterMisaligned(m)) = lower_with(sql, CatalogSource::InferredSeeded)
    {
        assert!(!m.contains("shape"), "expected no shape misalignment, got {m:?}");
    }
}

/// An observed pair's shape, reduced: both sides mention exactly `$1..$2`, so `arity` has nothing to
/// fire on and an `INSERT`'s `VALUES` parameters are compared against nothing, so `order` is vacuous —
/// yet `$1` is a `VARCHAR` on A and a `VARCHAR[]` on B. Under index binding the two sides cannot share
/// a binding at all, which is a statement about the question and not about what the frontend can lower.
#[test]
fn a_row_value_against_an_array_at_the_same_index_is_misaligned() {
    let sql = vpair(
        r#"INSERT INTO "v" ("a", "b") VALUES ($1, $2)"#,
        r#"INSERT INTO "v" ("a", "b") SELECT * FROM unnest($1::varchar[], $2::varchar[])"#,
    );
    misaligned(&sql, "shape");
}

/// The first control: two scalar sides. Same parameters, same roles, nothing misshapen — whatever else
/// happens to this pair, it is not this.
#[test]
fn two_row_value_sides_are_not_a_shape_misalignment() {
    not_misshapen(&vpair(
        r#"INSERT INTO "v" ("a", "b") VALUES ($1, $2)"#,
        r#"INSERT INTO "v" ("b", "a") VALUES ($2, $1)"#,
    ));
}

/// The second control, and the one that matters most: `unnest` on *both* sides. A disagreement needs
/// two readings that differ, so a parameter that is an array in both queries fires nothing. Without
/// this, the check would be testing for the presence of `unnest` rather than for a mismatch.
#[test]
fn an_array_on_both_sides_is_not_a_shape_misalignment() {
    not_misshapen(&vpair(
        r#"INSERT INTO "v" ("a", "b") SELECT * FROM unnest($1::varchar[], $2::varchar[])"#,
        r#"INSERT INTO "v" ("a", "b") SELECT "x", "y" FROM unnest($1::varchar[], $2::varchar[]) AS "u"("x", "y")"#,
    ));
}

/// A bare `unnest($1)` — no cast, because the parameter's declared type is already the array — reads as
/// an array too. The two branches of the array walk are not redundant: the cast one cannot see this.
#[test]
fn an_uncast_unnest_argument_is_still_an_array() {
    let sql = vpair(
        r#"INSERT INTO "v" ("a") VALUES ($1)"#,
        r#"INSERT INTO "v" ("a") SELECT * FROM unnest($1)"#,
    );
    misaligned(&sql, "shape");
}

/// The ordering claim, stated as a test: this sub-reason is raised ahead of `dml::reduce`, so a pair
/// that is *both* misshapen and an upsert reports the misalignment. That is the opposite of how `arity`
/// and `order` are ordered against a construct refusal, and `params::check_shape` argues why.
#[test]
fn a_shape_misalignment_is_reported_ahead_of_the_upsert_refusal() {
    let sql = vpair(
        r#"INSERT INTO "v" ("a", "b") VALUES ($1, $2) ON CONFLICT ("a") DO NOTHING"#,
        r#"INSERT INTO "v" ("a", "b") SELECT * FROM unnest($1::varchar[], $2::varchar[]) ON CONFLICT ("a") DO NOTHING"#,
    );
    misaligned(&sql, "shape");
}

/// The control for the test above: the upsert refusal is still there. Nothing misshapen, so nothing is
/// pre-empted, and the pair reports the construct exactly as it did before this check existed.
#[test]
fn an_upsert_with_nothing_misshapen_is_still_the_upsert() {
    let sql = vpair(
        r#"INSERT INTO "v" ("a", "b") VALUES ($1, $2) ON CONFLICT ("a") DO NOTHING"#,
        r#"INSERT INTO "v" ("b", "a") VALUES ($2, $1) ON CONFLICT ("a") DO NOTHING"#,
    );
    match lower_with(&sql, CatalogSource::InferredSeeded) {
        Err(FrontendError::Unsupported(m)) => assert!(m.contains("ON CONFLICT"), "{m}"),
        other => panic!("expected the upsert refusal, got {other:?}"),
    }
}

/// A parameter this walk finds in *both* roles inside one query is dropped rather than resolved: the
/// query's own business, and not evidence about a correspondence between two queries.
#[test]
fn a_parameter_used_both_ways_in_one_query_is_not_evidence() {
    not_misshapen(&vpair(
        r#"INSERT INTO "v" ("a") SELECT * FROM unnest($1::varchar[])"#,
        r#"INSERT INTO "v" ("a") VALUES ($1), (( SELECT "a" FROM unnest($1::varchar[]) AS "u"("a") LIMIT 1 ))"#,
    ));
}
