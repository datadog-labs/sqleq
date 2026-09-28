//! Self-contained tests (no corpus): each builds a tiny schema + pair and checks the verdict.

use std::collections::HashSet;

use sqleq_fuzz::duck::{ddl_for, table_forms};
use sqleq_fuzz::gen::{array_element_type, cast_target, CastTarget};
use sqleq_fuzz::patterns::{array_params, freeze_time, misalignment, param_casts, param_cols};
use sqleq_fuzz::schema::{parse_schema, parse_schema_stats, VType};
use sqleq_fuzz::typing::{param_needs, Need};
use sqleq_fuzz::{test_pair, Config, Verdict};

fn cfg() -> Config {
    Config {
        trials: 60,
        nrows: 5,
        seed: 0,
    }
}

fn label(a: &str, b: &str, ddl: &str) -> String {
    test_pair(a, b, ddl, cfg()).label()
}

fn names(ns: &[&str]) -> HashSet<String> {
    ns.iter().map(|s| s.to_string()).collect()
}

/// No array columns — the state every fixture without a `[]` declaration is in.
fn none() -> HashSet<String> {
    HashSet::new()
}

#[test]
fn nonequivalent_filter_is_flagged() {
    // On a row with a = 0, A returns it and B does not.
    let ddl = "CREATE TABLE t (a INTEGER, b INTEGER)";
    let v = test_pair("SELECT a FROM t", "SELECT a FROM t WHERE a > 0", ddl, cfg());
    assert!(
        matches!(v, Verdict::NotEquivalent(_)),
        "expected NOT-EQUIVALENT, got {}",
        v.label()
    );
}

#[test]
fn order_by_only_is_bag_equivalent() {
    // Bag semantics: an ORDER BY-only difference is not a counterexample.
    let ddl = "CREATE TABLE t (a INTEGER, b INTEGER)";
    assert_eq!(
        label(
            "SELECT a, b FROM t",
            "SELECT a, b FROM t ORDER BY a DESC",
            ddl
        ),
        "NO-COUNTEREXAMPLE"
    );
}

#[test]
fn equivalent_pair_not_flagged() {
    let ddl = "CREATE TABLE t (a INTEGER)";
    assert_eq!(
        label(
            "SELECT a FROM t WHERE a = 1",
            "SELECT a FROM t WHERE 1 = a",
            ddl
        ),
        "NO-COUNTEREXAMPLE"
    );
}

#[test]
fn a_width_changing_cast_is_not_a_counterexample() {
    // A declared `bigint` is materialized as INTEGER, so the cast side reads back as another
    // DuckDB type carrying the same value -- which used to be reported as a difference.
    let ddl = "CREATE TABLE t (id BIGINT PRIMARY KEY, parent BIGINT NOT NULL)";
    assert_eq!(
        label(
            "SELECT id, parent FROM t WHERE parent = $1",
            "SELECT id, $1::bigint AS parent FROM t WHERE parent = $1",
            ddl
        ),
        "NO-COUNTEREXAMPLE"
    );
    assert_eq!(
        label(
            "SELECT id, parent FROM t",
            "SELECT id, parent::numeric(12, 2) AS parent FROM t",
            ddl
        ),
        "NO-COUNTEREXAMPLE"
    );
    // A changed value still is one.
    let v = test_pair(
        "SELECT id, parent FROM t",
        "SELECT id, parent + 1 AS parent FROM t",
        ddl,
        cfg(),
    );
    assert!(
        matches!(v, Verdict::NotEquivalent(_)),
        "expected NOT-EQUIVALENT, got {}",
        v.label()
    );
}

#[test]
fn scalar_vs_grouped_aggregate_over_empty() {
    // The canonical bug pattern: `... WHERE a = $1 GROUP BY a` (no rows on empty match) vs the scalar
    // aggregate `... WHERE a = $1` (always one row). Value-from-data param biasing surfaces the split.
    let ddl = "CREATE TABLE t (a INTEGER, b INTEGER)";
    let v = test_pair(
        "SELECT count(b) FROM t WHERE a = $1 GROUP BY a",
        "SELECT count(b) FROM t WHERE a = $1",
        ddl,
        cfg(),
    );
    assert!(
        matches!(v, Verdict::NotEquivalent(_)),
        "expected NOT-EQUIVALENT, got {}",
        v.label()
    );
}

#[test]
fn no_schema_when_ddl_absent() {
    assert_eq!(label("SELECT 1", "SELECT 1", ""), "NO-SCHEMA");
}

#[test]
fn nondet_function_is_skipped() {
    let ddl = "CREATE TABLE t (a INTEGER)";
    assert_eq!(
        label("SELECT random() FROM t", "SELECT a FROM t", ddl),
        "NONDET-SKIP"
    );
}

#[test]
fn parse_schema_recovers_uniqueness() {
    // Column UNIQUE, table PRIMARY KEY, and CREATE UNIQUE INDEX must all become keys — missing any
    // would be unsound (we could fabricate an instance no valid DB admits).
    let s = parse_schema(
        "CREATE TABLE t (a INTEGER UNIQUE, b INTEGER, c INTEGER, PRIMARY KEY (b));\
         CREATE UNIQUE INDEX ix ON t (c)",
    );
    let t = s.get("t").expect("table t parsed");
    let has = |cols: &[&str]| {
        let want: Vec<String> = cols.iter().map(|c| c.to_string()).collect();
        t.keys.contains(&want)
    };
    assert!(has(&["a"]), "column UNIQUE key missing: {:?}", t.keys);
    assert!(has(&["b"]), "PRIMARY KEY missing: {:?}", t.keys);
    assert!(has(&["c"]), "UNIQUE INDEX key missing: {:?}", t.keys);
}

#[test]
fn not_null_is_parsed() {
    let s = parse_schema("CREATE TABLE t (a INTEGER NOT NULL, b INTEGER)");
    let t = &s["t"];
    assert!(t.cols[0].notnull, "a should be NOT NULL");
    assert!(!t.cols[1].notnull, "b should be nullable");
}

#[test]
fn locking_clause_is_stripped_inside_a_subquery() {
    // Most real row-locking sits inside a CTE/subquery, not at the end of the statement — an
    // end-anchored pattern misses it and DuckDB then rejects the whole query.
    let s = freeze_time("WITH c AS (SELECT id FROM t FOR UPDATE SKIP LOCKED) SELECT * FROM c");
    assert!(
        !s.to_uppercase().contains("FOR UPDATE"),
        "locking not stripped: {s}"
    );
    assert!(
        !s.to_uppercase().contains("SKIP LOCKED"),
        "SKIP LOCKED not stripped: {s}"
    );
    // The terminator that ended the clause must survive, or the parens no longer balance.
    assert!(
        s.contains(") SELECT * FROM c"),
        "terminator not restored: {s}"
    );
    // `FOR UPDATE OF <tables>` and a trailing semicolon are handled too.
    let s2 = freeze_time("SELECT a FROM t FOR UPDATE OF t NOWAIT;");
    assert!(
        !s2.to_uppercase().contains("FOR UPDATE"),
        "OF-list form not stripped: {s2}"
    );
}

#[test]
fn param_casts_pin_the_generated_type() {
    let casts = param_casts(
        "SELECT * FROM t WHERE d < now() - $1::interval",
        "SELECT $2::uuid",
    );
    assert_eq!(casts.get(&1).map(String::as_str), Some("interval"));
    assert_eq!(casts.get(&2).map(String::as_str), Some("uuid"));

    // Types with no VType analogue must classify to their own variant so they render as a castable
    // string rather than an integer.
    assert_eq!(cast_target("interval"), Some(CastTarget::Interval));
    assert_eq!(cast_target("jsonb"), Some(CastTarget::Json));
    // uuid has a generation domain of its own, so it is a VType rather than a string-shape target.
    assert_eq!(cast_target("uuid"), Some(CastTarget::V(VType::Uuid)));
    assert_eq!(
        cast_target("timestamp(6) without time zone"),
        Some(CastTarget::V(VType::Timestamp))
    );
    assert_eq!(cast_target("bigint"), Some(CastTarget::V(VType::Integer)));
    assert_eq!(cast_target("text"), Some(CastTarget::V(VType::Varchar)));
    // Shapes we refuse to guess at.
    assert_eq!(cast_target("text[]"), None);
    assert_eq!(cast_target("my_custom_enum"), None);
}

#[test]
fn interval_cast_param_does_not_error() {
    // Binding an integer to `$1::interval` fails the cast, which used to sink the whole pair to
    // ERROR. The pair below is equivalent, so the only way to get NO-COUNTEREXAMPLE is for the
    // param to bind as something an interval cast accepts.
    let ddl = "CREATE TABLE t (a INTEGER, d TIMESTAMP)";
    let v = test_pair(
        "SELECT a FROM t WHERE d < TIMESTAMP '2020-06-01' - $1::interval",
        "SELECT a FROM t WHERE d < TIMESTAMP '2020-06-01' - $1::interval",
        ddl,
        cfg(),
    );
    assert_eq!(
        v.label(),
        "NO-COUNTEREXAMPLE",
        "interval cast still failing: {}",
        v.label()
    );
}

#[test]
fn text_cast_does_not_displace_column_evidence() {
    // `WHERE int_col = $1::text` is common (ORM-generated). A text cast constrains nothing, so the
    // column must still decide the type — binding 'b' here makes DuckDB refuse the comparison and
    // sinks the pair to ERROR.
    let ddl = "CREATE TABLE t (id INTEGER, v INTEGER)";
    let v = test_pair(
        "SELECT v FROM t WHERE id = $1::text",
        "SELECT v FROM t WHERE id = $1",
        ddl,
        cfg(),
    );
    assert_eq!(v.label(), "NO-COUNTEREXAMPLE", "text cast broke the bind");
    assert!(
        v.partial().is_none(),
        "no trial should have errored: {:?}",
        v.partial()
    );
}

#[test]
fn partial_runs_are_not_reported_as_error() {
    // `$1` is compared to an INTEGER column (so binds as an int) but also cast to interval, which
    // cannot succeed for every trial. Some trials still run, so the pair must not be called ERROR.
    let ddl = "CREATE TABLE t (a INTEGER, d TIMESTAMP)";
    let v = test_pair(
        "SELECT a FROM t WHERE a = $1",
        "SELECT a FROM t WHERE a = $1",
        ddl,
        cfg(),
    );
    assert_eq!(v.label(), "NO-COUNTEREXAMPLE");
    assert!(
        v.partial().is_none(),
        "a fully-clean pair should not be marked partial"
    );
}

#[test]
fn a_misaligned_pair_yields_no_counterexample_claim() {
    // Under index binding B is strictly less selective, so the trials do find a difference — but the
    // row does not say that A's `$1` and B's `$1` are the same application value, and if the rewrite
    // dropped a leading placeholder they are not. The difference is then between two queries the caller
    // never paired, so it is not a counterexample and must not be reported as one.
    let ddl = "CREATE TABLE t (a INTEGER, b INTEGER)";
    let v = test_pair(
        "SELECT a FROM t WHERE a = $1 AND b = $2",
        "SELECT a FROM t WHERE a = $1",
        ddl,
        cfg(),
    );
    assert!(
        matches!(v, Verdict::ParamMisaligned(_)),
        "expected PARAM-MISALIGNED, got {}",
        v.label()
    );
    assert!(
        v.label().starts_with("PARAM-MISALIGNED:arity:"),
        "{}",
        v.label()
    );
}

#[test]
fn a_misaligned_pair_yields_no_passing_claim_either() {
    // The other direction, and the reason this is a withdrawal rather than a verdict: `$2` is a pure
    // row-limit param, bound large so it never truncates, so the two sides agree in every trial. That
    // agreement is evidence about index binding only — it says nothing about the pair the caller has.
    let ddl = "CREATE TABLE t (a INTEGER)";
    let v = test_pair(
        "SELECT a FROM t WHERE a = $1 LIMIT $2",
        "SELECT a FROM t WHERE a = $1",
        ddl,
        cfg(),
    );
    assert!(
        matches!(v, Verdict::ParamMisaligned(_)),
        "expected PARAM-MISALIGNED, got {}",
        v.label()
    );
}

#[test]
fn disjoint_parameter_sets_are_misaligned_too() {
    // The case the prover side deliberately exempts, and a disprover must not: sharing no index, the
    // trials draw `$1` and `$2` independently, hit two different values, and "disprove" a pair that is
    // equivalent for every caller who fills both from one value.
    let ddl = "CREATE TABLE t (a INTEGER)";
    let v = test_pair(
        "SELECT a FROM t WHERE a = $1",
        "SELECT a FROM t WHERE a = $2",
        ddl,
        cfg(),
    );
    assert!(
        matches!(v, Verdict::ParamMisaligned(_)),
        "expected PARAM-MISALIGNED, got {}",
        v.label()
    );
    assert!(v.label().contains("sharing none"), "{}", v.label());
}

#[test]
fn one_unparameterized_side_is_not_a_misalignment() {
    // Nothing is identified across the pair, so substituting values for the parameterized side's `$N`
    // *is* the caller's own quantification. A verdict is available and must still be given.
    let ddl = "CREATE TABLE t (a INTEGER)";
    assert_eq!(
        label("SELECT a FROM t LIMIT $1", "SELECT a FROM t", ddl),
        "NO-COUNTEREXAMPLE"
    );
    // Equal sets are the ordinary case, and the same-index-different-operand-order pair must survive.
    assert_eq!(
        label(
            "SELECT a FROM t WHERE a = $1",
            "SELECT a FROM t WHERE $1 = a",
            ddl
        ),
        "NO-COUNTEREXAMPLE"
    );
    assert!(misalignment("SELECT $1, $2", "SELECT $2, $1").is_none());
    assert!(misalignment("SELECT 1", "SELECT 2").is_none());
}

#[test]
fn a_misaligned_pair_that_never_ran_reports_why() {
    // `to_char` is a DuckDB gap, not a numbering problem: every trial fails before either side runs, so
    // there is no claim to withdraw and the fact worth reporting is the one a reader can act on.
    let ddl = "CREATE TABLE t (a INTEGER, b VARCHAR)";
    let v = test_pair(
        "SELECT a FROM t WHERE a = $1 AND b = $2 AND to_char(a, 'FM999') <> ''",
        "SELECT a FROM t WHERE a = $1 AND to_char(a, 'FM999') <> ''",
        ddl,
        cfg(),
    );
    assert!(
        misalignment("... $1 ... $2", "... $1 ...").is_some(),
        "the pair really is misaligned"
    );
    assert!(
        v.label().starts_with("ERROR"),
        "expected the untestability to win, got {}",
        v.label()
    );
}

#[test]
fn any_param_binds_an_array() {
    // `col = ANY($1)` takes an *array* in Postgres. Binding a scalar leaves DuckDB unnesting a
    // non-array, which sank a whole bucket of real pairs to ERROR before this lever.
    let ddl = "CREATE TABLE t (a INTEGER, b INTEGER)";
    let v = test_pair(
        "SELECT a FROM t WHERE a = ANY($1)",
        "SELECT a FROM t WHERE a = ANY($1)",
        ddl,
        cfg(),
    );
    assert_eq!(v.label(), "NO-COUNTEREXAMPLE");
    assert!(
        v.partial().is_none(),
        "some trials still failed: {:?}",
        v.partial()
    );
}

#[test]
fn any_param_array_is_selective_enough_to_discriminate() {
    // Binding an array is only useful if it still filters: an all-covering array would make the two
    // sides agree and hide the difference.
    let ddl = "CREATE TABLE t (a INTEGER)";
    let v = test_pair(
        "SELECT a FROM t WHERE a = ANY($1)",
        "SELECT a FROM t",
        ddl,
        cfg(),
    );
    assert!(
        matches!(v, Verdict::NotEquivalent(_)),
        "expected NOT-EQUIVALENT, got {}",
        v.label()
    );
}

#[test]
fn array_params_need_every_occurrence_inside_any() {
    // Mixed use: binding `$1` as a list would break the scalar comparison, so it must stay scalar.
    assert!(array_params(
        "SELECT 1 FROM t WHERE a = ANY($1) AND b = $1",
        "SELECT 1",
        &none()
    )
    .is_empty());
    // Purely array-valued, including through a `::T[]` cast; `$2` stays scalar.
    let p = array_params(
        "SELECT 1 FROM t WHERE a = ANY($1::uuid[]) AND b = $2",
        "SELECT 1",
        &none(),
    );
    assert!(p.contains(&1) && !p.contains(&2), "{p:?}");
    // `ANY(ARRAY[$1, $2])` passes scalars, not an array.
    assert!(array_params(
        "SELECT 1 FROM t WHERE a = ANY(ARRAY[$1, $2])",
        "SELECT 1",
        &none()
    )
    .is_empty());
    // The element type drives generation, since `cast_target` refuses `[]` names outright.
    assert_eq!(array_element_type("uuid[]"), "uuid");
    assert_eq!(
        cast_target(array_element_type("uuid[]").as_str()),
        Some(CastTarget::V(VType::Uuid))
    );
}

#[test]
fn uuid_columns_are_generated_as_uuid() {
    // A `uuid` column used to fall through to VARCHAR and hold 'a'/'b'/'c', which DuckDB then
    // refuses to compare against a uuid array at all ("Cannot compare values of type VARCHAR and
    // UUID in IN/ANY/ALL clause") — the pair never ran.
    let s = parse_schema("CREATE TABLE t (id UUID PRIMARY KEY, v INTEGER)");
    assert_eq!(s["t"].cols[0].vt, VType::Uuid);

    let ddl = "CREATE TABLE t (id UUID PRIMARY KEY, v INTEGER)";
    let v = test_pair(
        "SELECT v FROM t WHERE id = ANY($1::uuid[])",
        "SELECT v FROM t WHERE id = ANY($1::uuid[])",
        ddl,
        cfg(),
    );
    assert_eq!(v.label(), "NO-COUNTEREXAMPLE");
    assert!(
        v.partial().is_none(),
        "uuid array bind still failing: {:?}",
        v.partial()
    );

    // The generated values must reach rows, or the predicate is vacuous and discriminates nothing.
    let v = test_pair(
        "SELECT v FROM t WHERE id = ANY($1::uuid[])",
        "SELECT v FROM t",
        ddl,
        cfg(),
    );
    assert!(
        matches!(v, Verdict::NotEquivalent(_)),
        "expected NOT-EQUIVALENT, got {}",
        v.label()
    );

    // A scalar `$1::uuid` compared to the column has to bind from that column's data too.
    let v = test_pair(
        "SELECT v FROM t WHERE id = $1::uuid",
        "SELECT v FROM t",
        ddl,
        cfg(),
    );
    assert!(
        matches!(v, Verdict::NotEquivalent(_)),
        "expected NOT-EQUIVALENT, got {}",
        v.label()
    );
}

#[test]
fn in_subquery_params_belong_to_the_subquery() {
    // `outer_col IN (SELECT ... WHERE inner_col <> $1)`: `$1` is typed by `inner_col`. The IN-list
    // pattern stops at the first `)`, so it used to hand the *outer* column's type to every param in
    // the subquery — which binds a uuid into a VARCHAR comparison and makes DuckDB cast the column.
    let cols: HashSet<String> = ["control_id", "mitigation_type"]
        .iter()
        .map(|s| s.to_string())
        .collect();
    let m = param_cols(
        "SELECT 1 FROM d WHERE d.control_id IN (SELECT id FROM c WHERE mitigation_type <> $3)",
        "SELECT 1",
        &cols,
    );
    assert_eq!(
        m.get(&3).map(String::as_str),
        Some("mitigation_type"),
        "{m:?}"
    );
    // A genuine value list still links, including a cast one and a quoted identifier.
    let m = param_cols(
        "SELECT 1 FROM d WHERE d.control_id IN ($1, $2::uuid)",
        "SELECT 1",
        &cols,
    );
    assert_eq!(m.get(&1).map(String::as_str), Some("control_id"), "{m:?}");
    assert_eq!(m.get(&2).map(String::as_str), Some("control_id"), "{m:?}");

    // A value list *nested inside* a skipped subquery must still link. Matching the whole `(...)`
    // and rejecting it afterwards consumes the inner list too, leaving `$2` untyped — which is how
    // a uuid column ends up compared against the integer default.
    let cols: HashSet<String> = ["id", "org_id", "name"]
        .iter()
        .map(|s| s.to_string())
        .collect();
    let m = param_cols(
        "SELECT 1 FROM u WHERE u.id IN (SELECT r.id FROM r WHERE r.org_id IN ($2) AND r.name = $3)",
        "SELECT 1",
        &cols,
    );
    assert_eq!(m.get(&2).map(String::as_str), Some("org_id"), "{m:?}");
    assert_eq!(m.get(&3).map(String::as_str), Some("name"), "{m:?}");

    let ddl = "CREATE TABLE t (id UUID, v INTEGER); CREATE TABLE u (id UUID, name VARCHAR)";
    let q = "SELECT v FROM t WHERE t.id IN (SELECT u.id FROM u WHERE u.name <> $1)";
    let v = test_pair(q, q, ddl, cfg());
    assert_eq!(v.label(), "NO-COUNTEREXAMPLE");
    assert!(
        v.partial().is_none(),
        "param mistyped from the outer column: {:?}",
        v.partial()
    );
}

#[test]
fn freeze_time_freezes_runtime_clocks() {
    // Both parenthesised now() and the bare current_timestamp keyword must be frozen.
    assert!(!freeze_time("SELECT now()").to_lowercase().contains("now("));
    assert!(!freeze_time("SELECT current_timestamp")
        .to_lowercase()
        .contains("current_timestamp"));
    // Row-locking is stripped (result-irrelevant).
    assert!(!freeze_time("SELECT a FROM t FOR UPDATE")
        .to_uppercase()
        .contains("FOR UPDATE"));
}

#[test]
fn a_side_that_creates_objects_is_swept_between_trials() {
    // The trials of a pair share one DuckDB database (building one per side per trial cost ~20ms,
    // far more than the queries). `CREATE TABLE ... AS` leaves behind a table that is not one of
    // ours, so the cheap name-directed reset cannot see it — unswept, the next side's CREATE fails
    // with "already exists" and the pair reports ERROR instead of running.
    let ddl = "CREATE TABLE src (id INTEGER, v INTEGER)";
    let v = test_pair(
        "CREATE TABLE derived AS SELECT id, v FROM src WHERE v > 1",
        "CREATE TABLE derived AS SELECT id, v FROM src WHERE NOT (v <= 1)",
        ddl,
        cfg(),
    );
    assert_eq!(v.label(), "NO-COUNTEREXAMPLE");
    assert!(
        v.partial().is_none(),
        "objects leaked between trials: {:?}",
        v.partial()
    );
}

#[test]
fn writes_do_not_accumulate_across_trials() {
    // A DML pair is compared on final table state, so leaked rows would make every trial after the
    // first test a *different* instance than the one generated — and with a UNIQUE key the extra
    // rows start being rejected, which is how divergence would show up.
    let ddl = "CREATE TABLE t (id INTEGER, v INTEGER, UNIQUE (id)); \
               CREATE TABLE src (id INTEGER, v INTEGER)";
    let v = test_pair(
        "DELETE FROM t WHERE v > 1",
        "DELETE FROM t WHERE NOT (v <= 1)",
        ddl,
        cfg(),
    );
    assert_eq!(v.label(), "NO-COUNTEREXAMPLE");
    assert!(
        v.partial().is_none(),
        "rows accumulated across trials: {:?}",
        v.partial()
    );

    // And the final-state comparison really does discriminate — otherwise the check above would pass
    // for a table that was empty every trial.
    let v = test_pair(
        "DELETE FROM t WHERE v > 1",
        "DELETE FROM t WHERE v > 0",
        ddl,
        cfg(),
    );
    assert!(
        matches!(v, Verdict::NotEquivalent(_)),
        "expected NOT-EQUIVALENT, got {}",
        v.label()
    );
}

#[test]
fn batched_insert_keeps_exactly_the_valid_rows() {
    // Rows are inserted in one statement when nothing can violate a constraint, else one at a time.
    // The batch is only sound because a failing multi-row INSERT is atomic in DuckDB: the fallback
    // then reproduces the same survivor set. With a UNIQUE key over a 3-value domain, 5 generated
    // rows collide constantly, so this pair exercises the row-at-a-time path — and if a collision
    // ever aborted the whole insert, the two sides would see different instances.
    let ddl = "CREATE TABLE t (id INTEGER, v INTEGER, UNIQUE (id))";
    let v = test_pair(
        "SELECT count(*) FROM t",
        "SELECT count(1) FROM t",
        ddl,
        cfg(),
    );
    assert_eq!(v.label(), "NO-COUNTEREXAMPLE");
    assert!(v.partial().is_none(), "{:?}", v.partial());

    // The instance must not come out empty — that would make every such pair vacuously agree.
    let v = test_pair("SELECT count(*) FROM t", "SELECT 0", ddl, cfg());
    assert!(
        matches!(v, Verdict::NotEquivalent(_)),
        "table came back empty: {}",
        v.label()
    );
}

/// The reported instance must be the one the database actually held, not the rows the generator
/// proposed. `insert_rows` drops whatever violates UNIQUE or NOT NULL, so a witness printed from
/// the generated set shows duplicate keys under a UNIQUE the schema itself declares -- an instance
/// no valid database admits, and the first thing a reader checks against the DDL above it.
#[test]
fn the_reported_instance_satisfies_the_unique_constraint() {
    let ddl = "CREATE TABLE t (id INTEGER, v INTEGER, UNIQUE (id))";
    let v = test_pair("SELECT count(*) FROM t", "SELECT 0", ddl, cfg());
    let Verdict::NotEquivalent(ce) = &v else {
        panic!("expected a counterexample, got {}", v.label());
    };
    // `t=[(1,0); (NULL,2); (0,1)]` -- the first field of each tuple is `id`.
    let body = ce
        .split_once("t=[")
        .and_then(|(_, rest)| rest.split_once(']'))
        .map(|(body, _)| body)
        .unwrap_or_else(|| panic!("no instance in {ce:?}"));
    let ids: Vec<&str> = body
        .split(';')
        .map(|row| {
            row.trim()
                .trim_start_matches('(')
                .split(',')
                .next()
                .unwrap_or("")
                .trim()
        })
        .filter(|id| *id != "NULL")
        .collect();
    assert!(ids.len() >= 2, "instance too small to be a check: {ce}");
    let mut distinct = ids.clone();
    distinct.sort_unstable();
    distinct.dedup();
    assert_eq!(distinct.len(), ids.len(), "duplicate id in instance: {ce}");
}

/// A three-part wildcard qualifier is Postgres-legal and DuckDB-fatal (`syntax error at or near "*"`),
/// so before [`sqleq_fuzz::rewrite::unqualify_stars`] such a pair could only ever report an error — the
/// verdict was lost to a spelling. Both halves are pinned: the rewrite buys a real verdict, and it does
/// not buy it by weakening the comparison, since the same pair with a genuine difference under the same
/// wildcard is still caught.
#[test]
fn a_schema_qualified_wildcard_does_not_cost_the_verdict() {
    let ddl = "CREATE SCHEMA s; CREATE TABLE s.t (a INTEGER, b INTEGER)";
    assert_eq!(
        label(
            "SELECT s.t.* FROM s.t WHERE s.t.a = $1",
            "SELECT s.t.* FROM s.t WHERE $1 = s.t.a",
            ddl
        ),
        "NO-COUNTEREXAMPLE",
    );
    let v = test_pair(
        "SELECT s.t.* FROM s.t",
        "SELECT s.t.* FROM s.t WHERE s.t.a > 0",
        ddl,
        cfg(),
    );
    assert!(
        matches!(v, Verdict::NotEquivalent(_)),
        "expected NOT-EQUIVALENT, got {}",
        v.label()
    );
}

/// The typing pass has to be *consulted*, not merely correct: before it was wired in, a parameter no
/// column comparison reached was bound as an integer, and DuckDB rejected the statement at bind time
/// (`upper(INTEGER_LITERAL)`, `timezone(INTEGER_LITERAL, ...)`) — an ERROR where a verdict was
/// available. Both directions matter, so each shape is checked for a verdict it can reach either way.
#[test]
fn a_parameter_only_the_syntax_types_still_reaches_a_verdict() {
    let ddl = "CREATE TABLE t (s TEXT, ts TIMESTAMP)";
    for (a, b) in [
        (
            "SELECT s FROM t WHERE upper(s) = upper($1)",
            "SELECT s FROM t WHERE upper($1) = upper(s)",
        ),
        (
            "SELECT s FROM t WHERE (ts AT TIME ZONE $1) > TIMESTAMP '2020-01-01'",
            "SELECT s FROM t WHERE TIMESTAMP '2020-01-01' < (ts AT TIME ZONE $1)",
        ),
        (
            "SELECT s FROM t WHERE s LIKE $1",
            "SELECT s FROM t WHERE s LIKE $1 AND true",
        ),
    ] {
        assert_eq!(label(a, b, ddl), "NO-COUNTEREXAMPLE", "on {a}");
    }
    // And the same typing is what lets a real difference be found rather than reported as an error.
    let v = test_pair(
        "SELECT s FROM t WHERE upper(s) = upper($1)",
        "SELECT s FROM t WHERE upper(s) = upper($1) AND ts IS NULL",
        ddl,
        cfg(),
    );
    assert!(
        matches!(v, Verdict::NotEquivalent(_)),
        "expected NOT-EQUIVALENT, got {}",
        v.label()
    );
}

/// A DDL that declares two columns differing only in case is one Postgres would reject, so the table
/// the queries were written against had a single folded column. DuckDB folds identically and refuses
/// the whole `CREATE TABLE` if both are emitted, which cost the row its verdict.
#[test]
fn two_columns_differing_only_in_case_are_one_column() {
    let ddl = "CREATE TABLE t (id INTEGER PRIMARY KEY, modelID TEXT, modelId TEXT NOT NULL)";
    assert_eq!(
        label(
            "SELECT id FROM t WHERE modelId = $1",
            "SELECT id FROM t WHERE $1 = modelid",
            ddl
        ),
        "NO-COUNTEREXAMPLE"
    );
    // The surviving column keeps the stronger NOT NULL, so nothing widens the instance space: a
    // predicate that only a NULL could distinguish finds no counterexample.
    assert_eq!(
        label(
            "SELECT id FROM t WHERE modelId IS NULL",
            "SELECT id FROM t WHERE false",
            ddl
        ),
        "NO-COUNTEREXAMPLE"
    );
}

/// An `EXPLAIN` on either side means the statement returns a plan, not rows. Comparing a plan against
/// a result set — or, worse, two DuckDB plans against each other, which differ for reasons unrelated
/// to what the queries compute — would manufacture a counterexample about a rewrite that may be
/// perfectly sound. The pair gets no verdict instead.
#[test]
fn an_explained_side_is_not_compared() {
    let ddl = "CREATE TABLE t (id INTEGER PRIMARY KEY, v INTEGER)";
    let plain = "SELECT v FROM t WHERE id > $1";

    // One side explained: a real difference, about something nobody asked.
    let v = test_pair(&format!("EXPLAIN {plain}"), plain, ddl, cfg());
    assert_eq!(v.label(), "NOT-COMPARABLE:explain", "one side explained");

    // Both sides explained, and equivalent: comparing plan dumps is not comparing results.
    let v = test_pair(
        &format!("EXPLAIN {plain}"),
        "EXPLAIN SELECT v FROM t WHERE $1 < id",
        ddl,
        cfg(),
    );
    assert_eq!(v.label(), "NOT-COMPARABLE:explain", "both sides explained");

    // The guard is anchored to the statement, not to the word: a column named `explain` is fine.
    let v = test_pair(plain, "SELECT v FROM t WHERE id > $1", ddl, cfg());
    assert_eq!(v.label(), "NO-COUNTEREXAMPLE", "no explain anywhere");
}

// ---------------------------------------------------------------------------------------------
// Schema construction: four ways a perfectly testable pair used to be lost before either side ran.
// ---------------------------------------------------------------------------------------------

#[test]
fn a_key_on_an_undeclared_column_does_not_cost_the_table() {
    // Corpus DDL routinely indexes a column the (truncated) CREATE TABLE never lists. Emitting
    // `UNIQUE ("uuid")` fails the CREATE TABLE outright and every later reference reports the table
    // missing. The key is vacuous — no generated row can carry a value for a column that is not in
    // the instance — so dropping it weakens nothing.
    let ddl = "CREATE TABLE h (id INTEGER, name VARCHAR);\
               CREATE UNIQUE INDEX ix ON public.h USING btree (uuid)";
    let (s, dropped) = parse_schema_stats(ddl);
    assert_eq!(dropped, 1, "the unenforceable key should be counted");
    assert!(
        s["h"].keys.is_empty(),
        "unenforceable key kept: {:?}",
        s["h"].keys
    );
    assert_eq!(
        label("SELECT id FROM h", "SELECT h.id FROM h", ddl),
        "NO-COUNTEREXAMPLE"
    );
}

#[test]
fn an_enforceable_key_is_never_dropped() {
    // The soundness edge of the rule above: the test is exactly "column absent", never "key
    // inconvenient". A kept key is what stops us fabricating a two-row instance no DB admits.
    let ddl = "CREATE TABLE h (id INTEGER, name VARCHAR);\
               CREATE UNIQUE INDEX ix ON h (name)";
    let (s, dropped) = parse_schema_stats(ddl);
    assert_eq!(dropped, 0);
    assert_eq!(s["h"].keys, vec![vec!["name".to_string()]]);
}

#[test]
fn a_qualified_table_survives_a_query_that_does_not_parse() {
    // When either side fails to parse we fall back to regex. Recording the bare name only would
    // create `people` while the query reads `crm.people`, losing the pair to a missing schema
    // even though we hold the table.
    let s = parse_schema("CREATE TABLE people (id INTEGER)");
    let bad = "SELECT * FROM crm.people WHERE ?? id";
    let forms = table_forms(bad, bad, &s);
    assert_eq!(
        forms["people"].iter().cloned().collect::<Vec<_>>(),
        vec![vec!["crm".to_string(), "people".to_string()]],
        "expected the qualified form"
    );
}

#[test]
fn the_fallback_does_not_match_inside_a_longer_identifier() {
    let s = parse_schema("CREATE TABLE contacts (id INTEGER)");
    let bad = "SELECT * FROM user_contacts WHERE ?? id";
    assert!(
        table_forms(bad, bad, &s).is_empty(),
        "matched a suffix of a longer name"
    );
}

#[test]
fn a_cast_to_a_type_only_postgres_knows_still_reaches_a_verdict() {
    // The column is typed by the DDL, but the *cast* names a Postgres enum DuckDB has never heard
    // of. One connection serves both sides, so whatever we declare is identical for A and B.
    let ddl = "CREATE TABLE t (s VARCHAR, n INTEGER)";
    assert_eq!(
        label(
            "SELECT n FROM t WHERE s = 'x'::text::public.job_state",
            "SELECT n FROM t WHERE 'x'::text::public.job_state = s",
            ddl,
        ),
        "NO-COUNTEREXAMPLE"
    );
}

#[test]
fn a_builtin_cast_target_is_never_redeclared() {
    // The one way the rule above could go wrong: re-declaring a real type as VARCHAR silently
    // changes semantics. If `int8` became VARCHAR then `10 > 9` would compare as strings and A
    // would return nothing, so this pair would read NOT-EQUIVALENT.
    let ddl = "CREATE TABLE t (n INTEGER)";
    assert_eq!(
        label(
            "SELECT n FROM t WHERE 10::int8 > 9::int8",
            "SELECT n FROM t",
            ddl
        ),
        "NO-COUNTEREXAMPLE"
    );
}

#[test]
fn a_table_the_strict_parser_rejects_is_recovered() {
    // sqlparser rejects `primary` as an unquoted column name, and the whole table used to vanish —
    // surfacing much later, and unrecognisably, as "Table with name ... does not exist".
    let s = parse_schema(
        "CREATE TABLE clinical_roles (\
             care_case_id uuid NOT NULL,\
             created_at timestamp with time zone NOT NULL DEFAULT CURRENT_TIMESTAMP,\
             id BIGSERIAL PRIMARY KEY,\
             primary boolean NOT NULL DEFAULT false,\
             staffer_id uuid NOT NULL\
         )",
    );
    let t = s.get("clinical_roles").expect("table recovered");
    let names: Vec<&str> = t.cols.iter().map(|c| c.name.as_str()).collect();
    assert_eq!(
        names,
        vec!["care_case_id", "created_at", "id", "primary", "staffer_id"]
    );
    assert_eq!(t.cols[0].vt, VType::Uuid);
    assert_eq!(t.cols[1].vt, VType::Timestamp);
    assert_eq!(t.cols[2].vt, VType::Integer);
    assert_eq!(t.cols[3].vt, VType::Boolean);
    // Losing NOT NULL would widen the instance space, which is the unsound direction.
    assert!(
        t.cols.iter().all(|c| c.notnull),
        "every column here is NOT NULL: {:?}",
        t.cols
    );
    // `primary boolean` is a column, not a PRIMARY KEY: reading it as one abandons the table.
    assert_eq!(
        t.keys,
        vec![vec!["id".to_string()]],
        "only `id BIGSERIAL PRIMARY KEY` is a key"
    );
}

#[test]
fn recovery_keeps_the_constraints_it_finds() {
    let s = parse_schema(
        "CREATE TABLE q (primary boolean, a integer NOT NULL, b integer, UNIQUE (a, b))",
    );
    let t = s.get("q").expect("table recovered");
    assert_eq!(
        t.keys,
        vec![vec!["a".to_string(), "b".to_string()]],
        "{:?}",
        t.keys
    );
    assert!(t.cols[1].notnull);
}

#[test]
fn recovery_is_all_or_nothing() {
    // A uniqueness constraint we cannot read as a plain column list abandons the whole table: a
    // table recovered with a key silently missing is how a bogus counterexample gets manufactured.
    let s = parse_schema("CREATE TABLE q (primary boolean, b integer, UNIQUE (lower(b)))");
    assert!(
        !s.contains_key("q"),
        "recovered a table whose UNIQUE we could not read"
    );
    // Likewise a column definition we cannot read at all — `needs_review? boolean` is a shape
    // real DDL dumps do carry — half a table is worse than none.
    let s = parse_schema("CREATE TABLE q (primary boolean, needs_review? boolean)");
    assert!(
        !s.contains_key("q"),
        "recovered a table with an unreadable column"
    );
    // And `LIKE`, which inherits columns we cannot see.
    let s = parse_schema("CREATE TABLE q (LIKE other INCLUDING ALL, primary boolean)");
    assert!(
        !s.contains_key("q"),
        "recovered a table with inherited columns"
    );
}

#[test]
fn recovery_never_overrides_a_parsed_table() {
    let s = parse_schema("CREATE TABLE t (a INTEGER UNIQUE, b INTEGER)");
    assert_eq!(s["t"].cols.len(), 2);
    assert_eq!(s["t"].keys, vec![vec!["a".to_string()]]);
}

// --- jointly-constrained parameters: `c::text = $N` on one side, `c = $N::int4` on the other -----
//
// The rewrite these cover is the corpus's "cast the parameter, not the column" shape. It is *not*
// equivalence-preserving, and the only binding that shows it is text that is also a legal integer
// literal but not the canonical spelling of one.

fn needs_of(a: &str, b: &str, cols: &[(&str, VType)]) -> std::collections::HashMap<u32, Need> {
    let m = cols.iter().map(|(n, v)| (n.to_string(), *v)).collect();
    param_needs(a, b, &m)
}

#[test]
fn text_peer_against_int_cast_is_a_numeric_string() {
    // A reads `$1` as text (its peer is a `::varchar` expression); B casts it to int4. Neither
    // demand may be dropped: the value has to bind as text on A and survive `::int4` on B.
    let needs = needs_of(
        "SELECT id FROM t WHERE id::varchar = $1",
        "SELECT id FROM t WHERE id = $1::int4",
        &[("id", VType::Integer)],
    );
    assert_eq!(needs.get(&1), Some(&Need::NumericString));
}

#[test]
fn the_conflict_is_symmetric_in_the_sides() {
    // Same conflict with the text cast on B instead of A. Which query states which half must not
    // matter, or the verdict would depend on the corpus's column order.
    let needs = needs_of(
        "SELECT id FROM t WHERE id = $1::int4",
        "SELECT id FROM t WHERE id::varchar = $1",
        &[("id", VType::Integer)],
    );
    assert_eq!(needs.get(&1), Some(&Need::NumericString));
}

#[test]
fn numeric_and_double_casts_keep_the_cast() {
    // The dialect guard. `1::varchar` is `'1'` in both Postgres and DuckDB, but float and numeric
    // rendering is not portable (`1.50::text` keeps its scale in Postgres), so a difference found
    // this way could be DuckDB's formatting rather than the rewrite. Integer only.
    for cast in ["numeric", "float8", "double precision"] {
        let b = format!("SELECT x FROM t WHERE x = $1::{cast}");
        let needs = needs_of(
            "SELECT x FROM t WHERE x::varchar = $1",
            &b,
            &[("x", VType::Double)],
        );
        assert_ne!(
            needs.get(&1),
            Some(&Need::NumericString),
            "cast {cast} must not be widened"
        );
    }
}

#[test]
fn an_unconflicted_int_cast_is_unchanged() {
    // Nothing here reads `$1` as text, so the cast still wins outright: this rule is a carve-out
    // for a genuine conflict, not a new default for every `::int4`.
    let needs = needs_of(
        "SELECT id FROM t WHERE id = $1::int4",
        "SELECT id FROM t WHERE id = $1::int4",
        &[("id", VType::Integer)],
    );
    assert_eq!(
        needs.get(&1),
        Some(&Need::Type(CastTarget::V(VType::Integer)))
    );
}

#[test]
fn int_column_cast_rewrite_is_refuted() {
    // End to end. With `$1` bound to `'01'` and a row holding `id = 1`, A compares `'1' = '01'` and
    // returns nothing while B compares `1 = 1` and returns the row.
    let ddl = "CREATE TABLE t (id INTEGER)";
    let v = test_pair(
        "SELECT id FROM t WHERE id::text = $1",
        "SELECT id FROM t WHERE id = $1::int4",
        ddl,
        cfg(),
    );
    assert!(
        matches!(v, Verdict::NotEquivalent(_)),
        "expected NOT-EQUIVALENT, got {}",
        v.label()
    );
}

#[test]
fn text_column_cast_rewrite_is_not_refuted() {
    // The same *syntax* over a text column is a different pair: `c::text` is then a no-op and B
    // compares text to int4, which Postgres has no operator for. DuckDB will coerce the column
    // instead, and a verdict read off that coercion would describe a query Postgres rejects. The
    // trials error out and the pair stays undecided, which is the honest answer.
    let ddl = "CREATE TABLE t (c VARCHAR)";
    let label = label(
        "SELECT c FROM t WHERE c::text = $1",
        "SELECT c FROM t WHERE c = $1::int4",
        ddl,
    );
    assert!(
        !label.starts_with("NOT-EQUIVALENT"),
        "must not refute an ill-typed pair, got {label}"
    );
}

#[test]
fn array_columns_parse_as_element_type_plus_a_flag() {
    let s = parse_schema(
        "CREATE TABLE t (a TEXT[], b UUID[], c INTEGER[], d CHARACTER VARYING(64)[], \
         e TEXT, f TEXT[] DEFAULT '{}'::text[] NOT NULL)",
    );
    let cols = &s["t"].cols;
    let by = |n: &str| {
        cols.iter()
            .find(|c| c.name == n)
            .unwrap_or_else(|| panic!("no col {n}"))
    };
    // The element type is what `vt` holds; `array` carries the wrapping.
    assert_eq!((by("a").vt, by("a").array), (VType::Varchar, true));
    assert_eq!((by("b").vt, by("b").array), (VType::Uuid, true));
    assert_eq!((by("c").vt, by("c").array), (VType::Integer, true));
    assert_eq!((by("d").vt, by("d").array), (VType::Varchar, true));
    assert_eq!((by("e").vt, by("e").array), (VType::Varchar, false));
    // A `[]` inside a DEFAULT belongs to the default's cast, not to the column, and NOT NULL still
    // lands on the array as a whole.
    assert_eq!(
        (by("f").vt, by("f").array, by("f").notnull),
        (VType::Varchar, true, true)
    );

    // The declared type reaches DuckDB as a LIST. Generating it as a scalar VARCHAR is what made
    // every array operator over the column unbindable.
    let ddl = ddl_for(&["t".to_string()], &s["t"]);
    assert!(ddl.contains("\"a\" VARCHAR[]"), "{ddl}");
    assert!(ddl.contains("\"c\" INTEGER[]"), "{ddl}");
    assert!(ddl.contains("\"f\" VARCHAR[] NOT NULL"), "{ddl}");
    assert!(
        ddl.contains("\"e\" VARCHAR,"),
        "scalar column changed shape: {ddl}"
    );
}

#[test]
fn array_operators_type_a_param_only_with_array_evidence() {
    let tags = names(&["tags"]);
    let cols = names(&["tags", "id"]);

    // A whole operand of `&&`/`@>`/`<@` opposite an array column *is* an array, and links to that
    // column so its elements come from data the column actually holds.
    for q in [
        "SELECT id FROM t WHERE tags && $1",
        "SELECT id FROM t WHERE $1 <@ tags",
    ] {
        let p = array_params(q, "SELECT 1", &tags);
        assert!(p.contains(&1), "{q}: {p:?}");
        let m = param_cols(q, "SELECT 1", &cols);
        assert_eq!(m.get(&1).map(String::as_str), Some("tags"), "{q}: {m:?}");
    }

    // The same three spellings are jsonb containment and range overlap. Without array evidence the
    // rule declines: binding a list into `active_window @> $1::timestamp` would cost the pair its
    // run, and `$1` there is a scalar.
    for q in [
        "SELECT id FROM t WHERE active_window @> $1::timestamp",
        "SELECT id FROM t WHERE valid_period && $1::daterange",
        "SELECT id FROM t WHERE attrs_json @> $1",
    ] {
        assert!(array_params(q, "SELECT 1", &tags).is_empty(), "{q}");
    }

    // An explicit `::ty[]` cast is its own evidence, whatever the other operand is.
    let p = array_params(
        "SELECT 1 FROM t WHERE $1::text[] && whatever",
        "SELECT 1",
        &none(),
    );
    assert!(p.contains(&1), "{p:?}");

    // `ARRAY[$1]::text[] <@ tags`: `$1` is an *element*, so it stays scalar — but it must
    // still be drawn from `tags`, or the predicate matches nothing and discriminates nothing.
    let q = "SELECT id FROM t WHERE ARRAY[$1]::text[] <@ tags";
    assert!(
        array_params(q, "SELECT 1", &tags).is_empty(),
        "element param claimed as an array"
    );
    let m = param_cols(q, "SELECT 1", &cols);
    assert_eq!(m.get(&1).map(String::as_str), Some("tags"), "{m:?}");
    let m = param_cols(
        "SELECT id FROM t WHERE tags && ARRAY[$1, $2]",
        "SELECT 1",
        &cols,
    );
    assert_eq!(m.get(&1).map(String::as_str), Some("tags"), "{m:?}");
    assert_eq!(m.get(&2).map(String::as_str), Some("tags"), "{m:?}");

    // `$1 = ANY(tags)` is the other element shape: `$1` is compared *against* the array's members,
    // so it stays scalar and draws from them. `CMP_PARAM_COL` sees only the bare word `ANY` here.
    let q = "SELECT id FROM t WHERE $1::text = ANY(t.tags)";
    assert!(array_params(q, "SELECT 1", &tags).is_empty(), "element param claimed as an array");
    let m = param_cols(q, "SELECT 1", &cols);
    assert_eq!(m.get(&1).map(String::as_str), Some("tags"), "{m:?}");

    // The all-occurrences guard still holds: used as an array here and a scalar there, `$1` is
    // neither, because one bound value has to serve both.
    let p = array_params(
        "SELECT 1 FROM t WHERE tags && $1 AND id = $1",
        "SELECT 1",
        &tags,
    );
    assert!(p.is_empty(), "{p:?}");
}

#[test]
fn array_column_pairs_actually_run() {
    let ddl = "CREATE TABLE t (id INTEGER, tags TEXT[])";

    // The binding works at all: same query both sides, and `partial` empty proves every trial ran
    // rather than erroring on `&&(VARCHAR, VARCHAR[])`.
    let q = "SELECT id FROM t WHERE tags && $1";
    let v = test_pair(q, q, ddl, cfg());
    assert_eq!(v.label(), "NO-COUNTEREXAMPLE");
    assert!(
        v.partial().is_none(),
        "array bind still failing: {:?}",
        v.partial()
    );

    // And the generated arrays reach rows, so a genuine difference is found.
    let v = test_pair(q, "SELECT id FROM t", ddl, cfg());
    assert!(
        matches!(v, Verdict::NotEquivalent(_)),
        "expected NOT-EQUIVALENT, got {}",
        v.label()
    );

    // A shape seen in practice: containment one way, overlap with a singleton the other.
    // Equivalent for a non-NULL element, and the point of the test is that both sides bind and run.
    let v = test_pair(
        "SELECT id FROM t WHERE ARRAY[$1]::text[] <@ tags",
        "SELECT id FROM t WHERE tags && ARRAY[$1]::text[]",
        ddl,
        cfg(),
    );
    assert_eq!(v.label(), "NO-COUNTEREXAMPLE");
    assert!(
        v.partial().is_none(),
        "element param bind failing: {:?}",
        v.partial()
    );

    // A scalar element compared against the column's elements is discriminating, not vacuous --
    // in both spellings. Drawn from the integer default instead, neither would ever match a row.
    for q in [
        "SELECT id FROM t WHERE ARRAY[$1]::text[] <@ tags",
        "SELECT id FROM t WHERE $1::text = ANY(tags)",
    ] {
        let v = test_pair(q, "SELECT id FROM t", ddl, cfg());
        assert!(matches!(v, Verdict::NotEquivalent(_)), "{q}: got {}", v.label());
    }

    // An array param and an array column in the same predicate: `$1` stays a list (its `::text[]`
    // cast and `unnest` both say so) while the column contributes the elements it is compared
    // against. Array columns are kept out of the `param_needs` map for exactly this reason -- typed
    // from the column, `$1` would come out a scalar VARCHAR and `unnest` would refuse it.
    let q = "SELECT id FROM t \
             WHERE EXISTS (SELECT 1 FROM unnest($1::text[]) AS u(v) WHERE v = ANY(tags))";
    let v = test_pair(q, q, ddl, cfg());
    assert_eq!(v.label(), "NO-COUNTEREXAMPLE");
    assert!(v.partial().is_none(), "{:?}", v.partial());
}
