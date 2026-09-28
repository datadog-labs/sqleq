// Unless explicitly stated otherwise all files in this repository are licensed under the
// Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/).
// Copyright 2026-Present Datadog, Inc.

//! Integration tests for [`sqleq_frontend::reflexive`] — the check that says a pair's two sides
//! become the same tree under the crate's normalizations, so the row is settled without a schema,
//! a type, or the prover.
//!
//! Two properties are what these tests are for. **It reaches past a lowering refusal**: the whole
//! reason the check exists is that the frontend gives up on the first construct it cannot lower, so
//! a pair whose only difference is one a normalization erases is reported as unhandled. And **it
//! never widens a refusal into a proof**: every way it can fail answers `false`, and each guard the
//! normalizations carry has to still hold here.

use sqleq_frontend::{reflexive, reflexive_forms, reflexive_with, Rewrites};

/// A pair, in the `.sql` format the corpus driver builds.
fn pair(a: &str, b: &str) -> String {
    format!("{a};\n{b};")
}

#[test]
fn a_byte_identical_pair_is_reflexive() {
    assert!(reflexive(&pair("SELECT a FROM t", "SELECT a FROM t")));
}

/// Formatting and keyword case are not differences. This is where the AST check is *stronger* than
/// the textual predicate the suite uses to spot same-query-twice rows.
#[test]
fn formatting_and_keyword_case_are_not_differences() {
    assert!(reflexive(&pair("select   a\nfrom t", "SELECT a FROM t")));
}

#[test]
fn two_different_queries_are_not_reflexive() {
    assert!(!reflexive(&pair("SELECT a FROM t", "SELECT b FROM t")));
}

/// The lever this was built for: `x IN (SELECT DISTINCT e ...)` is `x IN (SELECT e ...)`, so a pair
/// whose only difference is the inserted `DISTINCT` is one query written twice.
#[test]
fn an_inserted_distinct_under_in_is_reflexive() {
    assert!(reflexive(&pair(
        "SELECT a FROM t WHERE a IN (SELECT DISTINCT x FROM u)",
        "SELECT a FROM t WHERE a IN (SELECT x FROM u)",
    )));
}

#[test]
fn an_inserted_distinct_under_exists_is_reflexive() {
    assert!(reflexive(&pair(
        "SELECT a FROM t WHERE EXISTS (SELECT DISTINCT x FROM u WHERE u.k = t.a)",
        "SELECT a FROM t WHERE EXISTS (SELECT x FROM u WHERE u.k = t.a)",
    )));
}

/// **The point of the whole change.** This pair cannot be lowered — a window function has no
/// counterpart in the prover's IR, so `lower_sql` refuses — and the refusal fires before anything
/// notices the two sides were already the same query.
#[test]
fn it_reaches_a_pair_the_frontend_refuses_to_lower() {
    let src = pair(
        "SELECT row_number() OVER (ORDER BY a) FROM t WHERE a IN (SELECT DISTINCT x FROM u)",
        "SELECT row_number() OVER (ORDER BY a) FROM t WHERE a IN (SELECT x FROM u)",
    );
    assert!(sqleq_frontend::lower_sql(&src).is_err(), "test is vacuous if this pair lowers");
    assert!(reflexive(&src));
}

/// The strip's own guard: `LIMIT` applies *after* `DISTINCT`, so removing the `DISTINCT` changes
/// which rows survive the slice. The guard has to hold here for the same reason it holds there.
#[test]
fn a_distinct_under_a_row_slice_is_not_reflexive() {
    assert!(!reflexive(&pair(
        "SELECT a FROM t WHERE a IN (SELECT DISTINCT x FROM u LIMIT 3)",
        "SELECT a FROM t WHERE a IN (SELECT x FROM u LIMIT 3)",
    )));
    assert!(!reflexive(&pair(
        "SELECT a FROM t WHERE a IN (SELECT DISTINCT x FROM u FETCH FIRST 3 ROWS ONLY)",
        "SELECT a FROM t WHERE a IN (SELECT x FROM u FETCH FIRST 3 ROWS ONLY)",
    )));
}

/// `DISTINCT ON` drops values rather than copies, so it is not the same rewrite at all.
#[test]
fn a_distinct_on_is_not_reflexive() {
    assert!(!reflexive(&pair(
        "SELECT a FROM t WHERE a IN (SELECT DISTINCT ON (x) x FROM u)",
        "SELECT a FROM t WHERE a IN (SELECT x FROM u)",
    )));
}

/// A `DISTINCT` anywhere but under `IN`/`EXISTS` is a real difference: at the top level it changes
/// the answer's multiplicity.
#[test]
fn a_top_level_distinct_is_not_reflexive() {
    assert!(!reflexive(&pair("SELECT DISTINCT a FROM t", "SELECT a FROM t")));
}

/// The query-level normalizations are reached too, so a pair differing only in whether its one CTE
/// is written out is reflexive.
#[test]
fn an_inlined_cte_is_reflexive() {
    assert!(reflexive(&pair(
        "WITH c AS (SELECT x FROM u) SELECT x FROM c",
        "SELECT x FROM (SELECT x FROM u) AS c",
    )));
}

/// `dml::reduce` is skipped, which is what makes a DML pair reachable at all: it raises on
/// `UPDATE ... RETURNING` before any normalization runs. Two identical `UPDATE`s are the same
/// statement whether or not anything reduces them.
#[test]
fn an_identical_dml_pair_is_reflexive() {
    let src = pair(
        "UPDATE t SET a = 1 WHERE a IN (SELECT DISTINCT x FROM u) RETURNING a",
        "UPDATE t SET a = 1 WHERE a IN (SELECT x FROM u) RETURNING a",
    );
    assert!(sqleq_frontend::lower_sql(&src).is_err(), "test is vacuous if this pair lowers");
    assert!(reflexive(&src));
    assert!(!reflexive(&pair("UPDATE t SET a = 1 WHERE a = 2", "UPDATE t SET a = 1 WHERE a = 3")));
}

/// Case *inside* a string literal is a difference, and the parser keeps it — so the AST comparison
/// gets this right without the special-casing the textual predicate needs.
#[test]
fn a_literal_that_differs_only_in_case_is_not_reflexive() {
    assert!(!reflexive(&pair("SELECT a FROM t WHERE a = 'A'", "SELECT a FROM t WHERE a = 'a'")));
}

/// Every way the check can fail answers `false`. It can turn a refusal into "the two sides are the
/// same query"; it can never turn one into a claim about two different ones.
#[test]
fn anything_that_is_not_a_pair_is_not_reflexive() {
    assert!(!reflexive("SELECT a FROM t;"));
    assert!(!reflexive(&format!("{};\n{};\n{};", "SELECT a FROM t", "SELECT a FROM t", "SELECT a FROM t")));
    assert!(!reflexive(&pair("SELECT FROM WHERE ,", "SELECT FROM WHERE ,")));
    assert!(!reflexive(""));
}

/// `CREATE TABLE`s in the input are the schema, not a side of the pair.
#[test]
fn declared_tables_are_not_counted_as_sides() {
    let src = format!("CREATE TABLE t (a INTEGER);\n{}", pair("SELECT a FROM t", "SELECT a FROM t"));
    assert!(reflexive(&src));
}

/// The single-subtraction counterfactual has to actually subtract something, or every attribution it
/// reports is vacuously "necessary". Both directions are pinned: clearing the bit that does the work
/// flips the verdict, and clearing an unrelated one does not.
#[test]
fn clearing_a_rewrite_disables_exactly_that_rewrite() {
    let p = pair("SELECT a FROM t WHERE b IN (SELECT c FROM u)",
                 "SELECT a FROM t WHERE b IN (SELECT DISTINCT c FROM u)");
    assert!(reflexive_with(&p, Rewrites::ALL));
    assert!(!reflexive_with(&p, Rewrites::ALL.without(Rewrites::STRIP_IN_EXISTS_DISTINCT)));
    assert!(reflexive_with(&p, Rewrites::ALL.without(Rewrites::STRIP_DEAD_ORDER_BY)));
    assert!(reflexive_with(&p, Rewrites::NONE.with(Rewrites::STRIP_IN_EXISTS_DISTINCT)));
    assert!(!reflexive_with(&p, Rewrites::NONE));
}

/// `Rewrites::ALL` is what ships, so it must name every bit an attribution pass can subtract -- a
/// rewrite missing from `EACH` would be reported as "no rewrite was necessary" for every row it closes.
#[test]
fn every_rewrite_is_reachable_from_all() {
    assert_eq!(Rewrites::EACH.len(), 10);
    let rebuilt = Rewrites::EACH.iter().fold(Rewrites::NONE, |acc, (_, b)| acc.with(*b));
    assert_eq!(rebuilt, Rewrites::ALL);
    for (_, bit) in Rewrites::EACH {
        assert_ne!(Rewrites::ALL.without(bit), Rewrites::ALL);
    }
}

/// The rendering a reader inspects by hand has to be the tree the verdict was computed from, not a
/// second normalization run that could drift from it.
#[test]
fn the_rendered_forms_agree_with_the_verdict() {
    let p = pair("SELECT a FROM t WHERE b IN (SELECT c FROM u)",
                 "SELECT a FROM t WHERE b IN (SELECT DISTINCT c FROM u)");
    let (a, b) = reflexive_forms(&p, Rewrites::ALL).expect("a pair");
    assert_eq!(a, b);
    let (a, b) = reflexive_forms(&p, Rewrites::NONE).expect("a pair");
    assert_ne!(a, b);
    assert_eq!(reflexive_forms("SELECT 1;", Rewrites::ALL), None);
}
