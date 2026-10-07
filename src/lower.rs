// Unless explicitly stated otherwise all files in this repository are licensed under the
// Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/).
// Copyright 2026-Present Datadog, Inc.

//! Lowering: SQL AST -> the prover's `Relation`/`Expr` IR (as `serde_json::Value`).
//!
//! Columns are lowered to absolute de-Bruijn indices (see [`crate::scope`]). Every clause that would
//! change the result and that we cannot lower faithfully is *refused* (returns `Err`) rather than
//! lowered best-effort — this is what preserves soundness end-to-end.

use std::collections::HashMap;
use std::ops::ControlFlow;

use serde_json::{json, Value};
use sqlparser::ast::visit_expressions;
use sqlparser::ast::{
    AccessExpr, BinaryOperator, Distinct, DuplicateTreatment, Expr, Function, FunctionArg, FunctionArgExpr,
    FunctionArgumentList, FunctionArguments, GroupByExpr, JoinConstraint, JoinOperator, OrderBy,
    Query,
    Select, SelectItem, SelectItemQualifiedWildcardKind, SetExpr, SetOperator, SetQuantifier,
    Subscript, TableFactor, TableWithJoins, UnaryOperator, Value as SqlValue, Values,
};

use crate::catalog::{obj_name, Catalog, FnDecl};
use crate::error::{schema, unsupported, Result};
use crate::scope::{is_unnamed, unnamed, Binding, Scope};
use crate::types::*;

/// Output columns of a (sub)query: `(name, prover type)`.
type OutCols = Vec<(String, String)>;

/// Functions declared by the `declare ... function` DSL, keyed by uppercased name.
type Fns = HashMap<String, FnDecl>;

/// The aggregates the prover models natively, and whose SQL semantics skip NULL inputs.
///
/// Both halves of that sentence are load-bearing and neither is negotiable, which is why
/// [`OPAQUE_AGGS`] is a separate list rather than more entries here: `ignoreNulls` and the `FILTER`
/// rewrite are both gated on membership, and both are only correct for a null-skipping aggregate the
/// prover actually interprets.
const BUILTIN_AGGS: [&str; 5] = ["COUNT", "SUM", "AVG", "MIN", "MAX"];

/// Aggregates the prover does not model, lowered as *uninterpreted aggregate* symbols — exactly what
/// a `declare aggregate function` line produces, without needing the line — paired with the type
/// they return.
///
/// SOUNDNESS GUARD. An aggregate on none of the lists here matches nothing: [`is_agg_call`] says no,
/// the query never takes the Group path, and the call is lowered as an ordinary per-row scalar. That
/// turns the one row an aggregate without `GROUP BY` returns into one row per input row — a
/// cardinality mis-lowering of the same class as [`SET_RETURNING`], reached from the other side — and
/// it is a false-proof channel: `SELECT count(*) FROM (SELECT var_pop(a) FROM t) q` returns 1 on an
/// empty `t`, and `SELECT count(*) FROM t` returns 0, yet the per-row reading made the two one query.
///
/// So every built-in Postgres aggregate is classified, and none is left to the scalar path: the
/// prover-native five are [`BUILTIN_AGGS`]; the ones whose result the bag of input values determines
/// are here; the ones it does not determine are [`ORDER_SENSITIVE_AGGS`] and [`UNMODELLED_AGGS`],
/// both refused. (An aggregate a user defines and the input does not declare is still a name nobody
/// can tell from a function's; see [`contains_agg`].)
///
/// An uninterpreted aggregate symbol is a *function of the bag*, so an entry here must be one:
/// `bool_or` and `bit_or` fold with an operation that is commutative, associative and idempotent,
/// `range_agg` and `range_intersect_agg` with union and intersection, and the statistical ones are
/// functions of sums over the bag -- exact sums over `numeric` and the integers. Over floating-point
/// input those sums round, and the rounding depends on the order the rows arrive in, so a call that
/// adds in floating point is refused instead ([`FLOAT_SUMMING_AGGS`], [`FLOAT_ONLY_AGGS`]).
///
/// The return types are Postgres's own where it has one: `bool_or`/`bool_and`/`every` return
/// `boolean`, `regr_count` returns `bigint`, and `corr`, `covar_*` and the other `regr_*` return
/// `double precision`, whatever they are given. Where the type follows the argument (`bit_and(int2)`
/// is `int2`, `var_pop(int)` is `numeric` and `var_pop(float8)` is `float8`, `range_agg` returns the
/// multirange of its range) the entry is [`UNDECLARED_RET`], opaque, for the reason that constant
/// gives. Null-handling is *not* asserted: `ignoreNulls` stays `false` and `FILTER` stays refused,
/// both of which are the incomplete-not-unsound direction.
///
/// The preprocessor refuses these instead, for a reason that does not apply here: sqlglot parses some
/// into dedicated node types that render as a plain call, so its downstream had no way to say
/// "aggregate". Nothing stops us saying it.
const OPAQUE_AGGS: [(&str, &str); 26] = [
    ("BOOL_OR", "BOOLEAN"),
    ("BOOL_AND", "BOOLEAN"),
    ("EVERY", "BOOLEAN"),
    ("BIT_AND", UNDECLARED_RET),
    ("BIT_OR", UNDECLARED_RET),
    ("BIT_XOR", UNDECLARED_RET),
    ("STDDEV", UNDECLARED_RET),
    ("STDDEV_POP", UNDECLARED_RET),
    ("STDDEV_SAMP", UNDECLARED_RET),
    ("VARIANCE", UNDECLARED_RET),
    ("VAR_POP", UNDECLARED_RET),
    ("VAR_SAMP", UNDECLARED_RET),
    ("CORR", "REAL"),
    ("COVAR_POP", "REAL"),
    ("COVAR_SAMP", "REAL"),
    ("REGR_AVGX", "REAL"),
    ("REGR_AVGY", "REAL"),
    ("REGR_COUNT", "INTEGER"),
    ("REGR_INTERCEPT", "REAL"),
    ("REGR_R2", "REAL"),
    ("REGR_SLOPE", "REAL"),
    ("REGR_SXX", "REAL"),
    ("REGR_SXY", "REAL"),
    ("REGR_SYY", "REAL"),
    ("RANGE_AGG", UNDECLARED_RET),
    ("RANGE_INTERSECT_AGG", UNDECLARED_RET),
];

/// Aggregates whose result is not determined by the bag of values they fold over.
///
/// SOUNDNESS GUARD, and the reason [`OPAQUE_AGGS`] cannot simply be extended with them.
///
/// An uninterpreted aggregate symbol is a *function of the bag*: the prover assumes nothing about
/// what it computes, but it does assume that feeding it equal bags gives equal results. That is what
/// makes `bool_or` safe up there — disjunction is commutative, associative and idempotent, so the bag
/// really does determine the answer.
///
/// These do not have that property. `array_agg(v)` over the bag `{a, b}` is `[a, b]` or `[b, a]`
/// depending on the order rows reach the aggregate, which SQL leaves unspecified and which the plan
/// decides; `string_agg` and the `json*_agg` family are the same. Modelling one as a function of the
/// bag therefore asserts an equality Postgres does not honour — and asserting *more* equalities than
/// reality is the unsound direction, because a rewrite that preserves the bag while changing the
/// order (join reordering, a different index) would come out provably equivalent when it is not.
///
/// The alternative is not "model them anyway": within bag semantics, where a relation has no order at
/// all, an order-sensitive aggregate is simply not expressible. Refusing is how that is said.
///
/// Refusing also fixes a second, independent defect these had while they were unlisted. Being absent
/// from both aggregate lists, [`is_agg_call`] said no, the query never took the Group path, and
/// `SELECT array_agg(v) FROM t` lowered to a *per-row scalar* over the scan — N output rows where SQL
/// returns exactly one. That is the [`OPAQUE_AGGS`] cardinality bug over again, and it was live: four
/// corpus cases lowered that way and all four proved. None of the four is a false verdict — each is a
/// `DISTINCT`-inside-`IN` rewrite that `normalize::strip_in_exists_distinct` makes reflexive, so both
/// sides carried the identical wrong shape and the theorem was `X ≡ X` — but that is luck, not a
/// property, and those four are exactly the reflexive pairs that measure no capability.
///
/// The preprocessor demotes these to `qa_*` aggregate symbols instead, which fixes the cardinality
/// and takes on the bag-determinism assumption. That is the trade being declined here.
///
/// The `_strict` and `_unique` variants of the `json*_agg` family are here with their base forms, and
/// so are the SQL/JSON spellings `json_arrayagg` and `json_objectagg`.
const ORDER_SENSITIVE_AGGS: [&str; 19] = [
    "ARRAY_AGG",
    "STRING_AGG",
    "GROUP_CONCAT",
    "LISTAGG",
    "JSON_AGG",
    "JSON_AGG_STRICT",
    "JSONB_AGG",
    "JSONB_AGG_STRICT",
    "JSON_OBJECT_AGG",
    "JSON_OBJECT_AGG_STRICT",
    "JSON_OBJECT_AGG_UNIQUE",
    "JSON_OBJECT_AGG_UNIQUE_STRICT",
    "JSONB_OBJECT_AGG",
    "JSONB_OBJECT_AGG_STRICT",
    "JSONB_OBJECT_AGG_UNIQUE",
    "JSONB_OBJECT_AGG_UNIQUE_STRICT",
    "JSON_ARRAYAGG",
    "JSON_OBJECTAGG",
    "XMLAGG",
];

/// Aggregates that add their inputs in floating point when one of them is a float, and whose result
/// then depends on the order the rows arrive in.
///
/// SOUNDNESS GUARD, for the reason [`ORDER_SENSITIVE_AGGS`] gives. Float addition rounds, so it is
/// not associative: `(1e20 + 1) + -1e20` is `0` and `(-1e20 + 1e20) + 1` is `1`. Over `real` or
/// `double precision`, `sum` and `avg` add the values in the order the rows reach the aggregate, and
/// the `stddev` and `var` family accumulate their sums the same way. A rewrite that keeps the bag and
/// changes the order -- the two branches of a `UNION ALL` swapped, a subquery's `ORDER BY` -- changes
/// the result, while the prover's `sum`, or an uninterpreted aggregate, is a function of the bag and
/// proves it unchanged. So a call to one of these over a float is refused, in aggregate position;
/// elsewhere an aggregate is refused anyway. Over `numeric` and the integer types each of them adds
/// exactly, and the bag determines the result.
///
/// Refused wherever it is lowered, never lifted because the two queries lowered to one plan: the
/// lowering drops a subquery's `ORDER BY` that no row slice reads, so two queries that sort the rows
/// they add in two orders lower to one plan.
const FLOAT_SUMMING_AGGS: [&str; 8] =
    ["SUM", "AVG", "STDDEV", "STDDEV_POP", "STDDEV_SAMP", "VARIANCE", "VAR_POP", "VAR_SAMP"];

/// Aggregates Postgres declares over `double precision` only, which compute in floating point whatever
/// they are given: an integer or `numeric` argument is converted to a float first. Refused wherever
/// they are lowered, as [`FLOAT_SUMMING_AGGS`] are over a float. `regr_count`, which counts, is not
/// here.
const FLOAT_ONLY_AGGS: [&str; 11] = [
    "CORR",
    "COVAR_POP",
    "COVAR_SAMP",
    "REGR_AVGX",
    "REGR_AVGY",
    "REGR_INTERCEPT",
    "REGR_R2",
    "REGR_SLOPE",
    "REGR_SXX",
    "REGR_SXY",
    "REGR_SYY",
];

/// The other built-in aggregates that are not a function of the bag of values they fold over, or that
/// cannot be called without a clause the lowering does not read.
///
/// SOUNDNESS GUARD, refused wherever a call is lowered, as [`ORDER_SENSITIVE_AGGS`] is and for the
/// same two reasons: in aggregate position the bag would be taken to determine the result, and in
/// scalar position, where these used to land, the cardinality is wrong outright.
///
/// * `any_value` returns an arbitrary one of its inputs, so two calls over one bag need not agree.
/// * `mode`, `percentile_cont` and `percentile_disc` are ordered-set aggregates, and `rank`,
///   `dense_rank`, `percent_rank` and `cume_dist` (without `OVER`) hypothetical-set ones: the values
///   they fold over are in `WITHIN GROUP (ORDER BY ...)`, which [`call_parts`] refuses anyway.
const UNMODELLED_AGGS: [&str; 8] = [
    "ANY_VALUE",
    "MODE",
    "PERCENTILE_CONT",
    "PERCENTILE_DISC",
    "RANK",
    "DENSE_RANK",
    "PERCENT_RANK",
    "CUME_DIST",
];

/// The functions Postgres declares `VOLATILE`: their result can differ between two calls with the
/// same arguments. Lowercase, as Postgres spells them, and sorted.
///
/// SOUNDNESS GUARD. Every other unknown call is modelled as an uninterpreted *function*, and the
/// whole force of that word is that equal arguments give equal results — which is what lets both
/// sides of a rewrite share one symbol. These do not have that property: `random()` twice is two
/// values, `nextval` advances a sequence, `clock_timestamp()` moves during the statement. Modelling
/// one as a function asserts an equality the database does not honour, so a pair that differs only
/// in how many times it calls one would come out equivalent. Lowering refuses every call to one, and
/// [`reflexive`](crate::reflexive) declines a pair where inlining a `WITH` binding would copy one.
///
/// The list is every function `pg_proc` marks volatile (`provolatile = 'v'`) in Postgres 17, plus the
/// volatile functions of the `pgcrypto` and `uuid-ossp` extensions, plus `uuidv4` and `uuidv7`
/// (Postgres 18) and the `pg_uuidv7` extension's `uuid_generate_v7`. Left out are the functions whose
/// result type no call in a query can produce (`trigger`, `event_trigger`, `internal` and the
/// `*_handler` types). It is matched on a call's unqualified name in any case, which can only refuse
/// more than Postgres would. It is a denylist all the same: a volatile function a user defines is a
/// name nobody can tell from any other.
///
/// Not to be confused with the statement-stable clocks — `now()`, `current_timestamp`,
/// `transaction_timestamp()`, `localtimestamp` — which are fixed for the duration of a statement and
/// so *are* faithful as shared constants. Postgres declares them `STABLE`, and they are absent here.
///
/// Public so that it is the one list of its kind: `sqleq-lean` reads it, and `sqleq-fuzz`, which
/// does not link this crate, keeps its skip pattern in step with it.
pub const VOLATILE_FUNCTIONS: &[&str] = &[
    "amvalidate", "array_sample", "array_shuffle", "binary_upgrade_add_sub_rel_state",
    "binary_upgrade_create_empty_extension", "binary_upgrade_logical_slot_has_caught_up",
    "binary_upgrade_replorigin_advance", "binary_upgrade_set_missing_value",
    "binary_upgrade_set_next_array_pg_type_oid", "binary_upgrade_set_next_heap_pg_class_oid",
    "binary_upgrade_set_next_heap_relfilenode", "binary_upgrade_set_next_index_pg_class_oid",
    "binary_upgrade_set_next_index_relfilenode",
    "binary_upgrade_set_next_multirange_array_pg_type_oid",
    "binary_upgrade_set_next_multirange_pg_type_oid", "binary_upgrade_set_next_pg_authid_oid",
    "binary_upgrade_set_next_pg_enum_oid", "binary_upgrade_set_next_pg_tablespace_oid",
    "binary_upgrade_set_next_pg_type_oid", "binary_upgrade_set_next_toast_pg_class_oid",
    "binary_upgrade_set_next_toast_relfilenode", "binary_upgrade_set_record_init_privs",
    "brin_desummarize_range", "brin_summarize_new_values", "brin_summarize_range",
    "clock_timestamp", "current_query", "currtid2", "currval", "cursor_to_xml",
    "cursor_to_xmlschema", "gen_random_bytes", "gen_random_uuid", "gen_salt",
    "gin_clean_pending_list", "lastval", "lo_close", "lo_creat", "lo_create", "lo_export",
    "lo_from_bytea", "lo_get", "lo_import", "lo_lseek", "lo_lseek64", "lo_open", "lo_put",
    "lo_tell", "lo_tell64", "lo_truncate", "lo_truncate64", "lo_unlink", "loread", "lowrite",
    "nextval", "pg_advisory_lock", "pg_advisory_lock_shared", "pg_advisory_unlock",
    "pg_advisory_unlock_all", "pg_advisory_unlock_shared", "pg_advisory_xact_lock",
    "pg_advisory_xact_lock_shared", "pg_available_wal_summaries", "pg_backup_start",
    "pg_backup_stop", "pg_blocking_pids", "pg_cancel_backend", "pg_collation_actual_version",
    "pg_control_checkpoint", "pg_control_init", "pg_control_recovery", "pg_control_system",
    "pg_copy_logical_replication_slot", "pg_copy_physical_replication_slot",
    "pg_create_logical_replication_slot", "pg_create_physical_replication_slot",
    "pg_create_restore_point", "pg_current_logfile", "pg_current_wal_flush_lsn",
    "pg_current_wal_insert_lsn", "pg_current_wal_lsn", "pg_database_collation_actual_version",
    "pg_database_size", "pg_drop_replication_slot", "pg_export_snapshot",
    "pg_extension_config_dump", "pg_get_backend_memory_contexts", "pg_get_multixact_members",
    "pg_get_shmem_allocations", "pg_get_wait_events", "pg_get_wal_replay_pause_state",
    "pg_get_wal_resource_managers", "pg_get_wal_summarizer_state", "pg_hba_file_rules",
    "pg_ident_file_mappings", "pg_import_system_collations", "pg_indexes_size", "pg_is_in_recovery",
    "pg_is_wal_replay_paused", "pg_isolation_test_session_is_blocked", "pg_jit_available",
    "pg_last_committed_xact", "pg_last_wal_receive_lsn", "pg_last_wal_replay_lsn",
    "pg_last_xact_replay_timestamp", "pg_lock_status", "pg_log_backend_memory_contexts",
    "pg_log_standby_snapshot", "pg_logical_emit_message", "pg_logical_slot_get_binary_changes",
    "pg_logical_slot_get_changes", "pg_logical_slot_peek_binary_changes",
    "pg_logical_slot_peek_changes", "pg_ls_archive_statusdir", "pg_ls_dir", "pg_ls_logdir",
    "pg_ls_logicalmapdir", "pg_ls_logicalsnapdir", "pg_ls_replslotdir", "pg_ls_tmpdir",
    "pg_ls_waldir", "pg_nextoid", "pg_notification_queue_usage", "pg_notify",
    "pg_partition_ancestors", "pg_partition_tree", "pg_prepared_xact", "pg_promote",
    "pg_read_binary_file", "pg_read_file", "pg_relation_size", "pg_reload_conf",
    "pg_replication_origin_advance", "pg_replication_origin_create", "pg_replication_origin_drop",
    "pg_replication_origin_progress", "pg_replication_origin_session_is_setup",
    "pg_replication_origin_session_progress", "pg_replication_origin_session_reset",
    "pg_replication_origin_session_setup", "pg_replication_origin_xact_reset",
    "pg_replication_origin_xact_setup", "pg_replication_slot_advance", "pg_rotate_logfile",
    "pg_safe_snapshot_blocking_pids", "pg_sequence_last_value", "pg_show_all_file_settings",
    "pg_show_replication_origin_status", "pg_sleep", "pg_sleep_for", "pg_sleep_until",
    "pg_stat_clear_snapshot", "pg_stat_file", "pg_stat_force_next_flush", "pg_stat_get_io",
    "pg_stat_get_recovery_prefetch", "pg_stat_get_xact_blocks_fetched",
    "pg_stat_get_xact_blocks_hit", "pg_stat_get_xact_function_calls",
    "pg_stat_get_xact_function_self_time", "pg_stat_get_xact_function_total_time",
    "pg_stat_get_xact_numscans", "pg_stat_get_xact_tuples_deleted",
    "pg_stat_get_xact_tuples_fetched", "pg_stat_get_xact_tuples_hot_updated",
    "pg_stat_get_xact_tuples_inserted", "pg_stat_get_xact_tuples_newpage_updated",
    "pg_stat_get_xact_tuples_returned", "pg_stat_get_xact_tuples_updated", "pg_stat_have_stats",
    "pg_stat_reset", "pg_stat_reset_replication_slot", "pg_stat_reset_shared",
    "pg_stat_reset_single_function_counters", "pg_stat_reset_single_table_counters",
    "pg_stat_reset_slru", "pg_stat_reset_subscription_stats", "pg_stop_making_pinned_objects",
    "pg_switch_wal", "pg_sync_replication_slots", "pg_table_size", "pg_tablespace_size",
    "pg_terminate_backend", "pg_total_relation_size", "pg_try_advisory_lock",
    "pg_try_advisory_lock_shared", "pg_try_advisory_xact_lock", "pg_try_advisory_xact_lock_shared",
    "pg_wal_replay_pause", "pg_wal_replay_resume", "pg_wal_summary_contents",
    "pg_xact_commit_timestamp", "pg_xact_commit_timestamp_origin", "pg_xact_status",
    "pgp_pub_encrypt", "pgp_pub_encrypt_bytea", "pgp_sym_encrypt", "pgp_sym_encrypt_bytea",
    "plpgsql_inline_handler", "plpgsql_validator", "query_to_xml", "query_to_xml_and_xmlschema",
    "query_to_xmlschema", "random", "random_normal", "set_config", "setseed", "setval", "timeofday",
    "ts_rewrite", "ts_stat", "txid_status", "uuid_generate_v1", "uuid_generate_v1mc",
    "uuid_generate_v4", "uuid_generate_v7", "uuidv4", "uuidv7",
];

/// Set-returning functions: the ones that expand one input row into *many* output rows.
///
/// Every other unknown function is lowered as an uninterpreted scalar, which is faithful because a
/// function is a function — whatever it computes, it computes one value and both queries get the
/// same symbol. These break that: `SELECT EXPLODE(a) FROM t` returns one row per array element, not
/// one per input row, so modelling the call as a scalar understates the cardinality. That is
/// invisible when both sides use it identically but not otherwise (`SELECT DISTINCT EXPLODE(a)`
/// against `SELECT EXPLODE(a)` would come out equal on a one-row table and are not), so they are
/// refused in scalar position rather than mis-modelled.
const SET_RETURNING: [&str; 14] = [
    "EXPLODE",
    "EXPLODE_OUTER",
    "POSEXPLODE",
    "POSEXPLODE_OUTER",
    "INLINE",
    "INLINE_OUTER",
    "UNNEST",
    "GENERATE_SERIES",
    "GENERATE_SUBSCRIPTS",
    "JSON_ARRAY_ELEMENTS",
    "JSON_ARRAY_ELEMENTS_TEXT",
    "JSONB_ARRAY_ELEMENTS",
    "JSONB_ARRAY_ELEMENTS_TEXT",
    "REGEXP_SPLIT_TO_TABLE",
];

/// The result type assumed for a call nobody declared.
///
/// `VARBINARY` is opaque here ([`is_builtin`] is false for it), so [`common_type`] lets it win a
/// coercion and [`make_arith`] emits an *uninterpreted* `+` over an uninterpreted sort. `INTEGER`
/// — what this used to be — instead hands the prover genuine integer arithmetic and a total order.
///
/// The opaque choice is the faithful one whichever type the function really returns. If it really
/// returns an integer, the uninterpreted reading is an abstraction of the integer one: every real
/// behaviour is still among the modelled interpretations. If it returns a timestamp or text — which
/// is what these calls mostly are, `TIMESTAMP_TRUNC` and friends — the integer reading asserts laws
/// that do not hold of the real function, and the prover is reasoning about a program that does not
/// exist. `INTEGER` is only faithful in the first case; `VARBINARY` is faithful in both.
///
/// This was measured rather than assumed. Over a corpus of lowered cases, switching the default from
/// one to the other moved **zero verdicts** — the same pairs proved, case by case and not merely in
/// total. The weaker assumption is free here, so it is taken.
///
/// It is not faithful by itself for `=`, which both provers read as identity on VARBINARY: the
/// function may return a `numeric` or a float, two of whose values `=` calls equal and a cast to text
/// tells apart (`round(i, 1)` and `round(i, 2)`, `-sqrt(0)` and `sqrt(0)`). So the frontend reads
/// plain VARBINARY as a value whose `=` is not known to be identity ([`coarse_class`]), and
/// [`crate::equality`] refuses an operation that could tell two equal ones apart.
const UNDECLARED_RET: &str = "VARBINARY";

/// The two names a call answers to: its qualified spelling and its bare final component.
///
/// The distinction is load-bearing, because the name does two unrelated jobs.
///
/// As the **IR operator** it is the *identity* of an uninterpreted symbol, and must stay qualified:
/// collapsing `sales.total` and `hr.total` into one `TOTAL` would let the prover assume two
/// different functions are the same function, which is a false-proof channel.
///
/// As a **declaration key** it names a function, not a call site. `declare scalar function f(...)`
/// describes `f` however it is spelled at the call, so the lookup has to reach it from a qualified
/// call too. The preprocessor makes this concrete: sqlglot parses `pg_catalog.like_escape(a, b)`
/// into an `Anonymous` whose `.name` is the bare `like_escape` — which is what it writes into the
/// `declare` line — but renders the call back *qualified*. Keying only on the qualified spelling
/// throws that declaration away.
fn fn_names(f: &Function) -> (String, String) {
    let full = obj_name(&f.name).to_uppercase();
    (full.clone(), bare_name(&full).to_string())
}

/// The final component of a possibly-qualified name: `PG_CATALOG.LIKE_ESCAPE` -> `LIKE_ESCAPE`.
fn bare_name(full: &str) -> &str {
    full.rsplit('.').next().unwrap_or(full)
}

/// The type an [`OPAQUE_AGGS`] entry returns, if `name` is one.
fn opaque_agg_ret(name: &str) -> Option<&'static str> {
    OPAQUE_AGGS.iter().find(|(n, _)| *n == name).map(|(_, t)| *t)
}

/// Whether `name` is an aggregate this frontend knows about without being told: prover-native
/// ([`BUILTIN_AGGS`]) or uninterpreted ([`OPAQUE_AGGS`]).
fn is_known_agg(name: &str) -> bool {
    BUILTIN_AGGS.contains(&name) || opaque_agg_ret(name).is_some()
}

/// The declaration for a call: exact qualified name first, then the bare component.
///
/// Order matters. An exact match is a statement about *this* function and always wins; the bare
/// fallback is a weaker inference, used only when nothing was declared under the qualified name.
fn fn_decl<'a>(fns: &'a Fns, full: &str, bare: &str) -> Option<&'a FnDecl> {
    fns.get(full).or_else(|| fns.get(bare))
}

/// The declared return type of a function, or the default when it wasn't declared.
fn fn_ret(fns: &Fns, full: &str, bare: &str) -> String {
    fn_decl(fns, full, bare).map(|d| d.ret.clone()).unwrap_or_else(|| UNDECLARED_RET.to_string())
}

/// SOUNDNESS GUARD: a qualified call whose bare name is one of the aggregates we know about cannot
/// be classified, so it is refused rather than guessed at.
///
/// Neither reading is safe. Treating `pg_catalog.sum(x)` as the builtin asserts real summation
/// semantics for a symbol that, under some other schema, may be an unrelated user function.
/// Treating it as an ordinary scalar is worse: an aggregate on the scalar path becomes a per-row
/// function and turns one output row into one row per input row. Refusing costs completeness on a
/// spelling nobody writes by accident.
fn reject_qualified_builtin_agg(full: &str, bare: &str) -> Result<()> {
    if full != bare && is_known_agg(bare) {
        return Err(unsupported(format!("qualified builtin aggregate {full} (cannot classify)")));
    }
    Ok(())
}

/// Whether `name`, a function's unqualified name in any case, is on [`VOLATILE_FUNCTIONS`].
pub fn is_volatile(name: &str) -> bool {
    VOLATILE_FUNCTIONS.iter().any(|v| v.eq_ignore_ascii_case(name))
}

/// SOUNDNESS GUARD: see [`VOLATILE_FUNCTIONS`]. Matched on the *bare* name, because
/// `pg_catalog.random` is still `random` and widening a refusal can only ever cost completeness.
fn reject_nondeterministic(full: &str, bare: &str) -> Result<()> {
    if is_volatile(bare) {
        return Err(unsupported(format!("non-deterministic function {full}")));
    }
    Ok(())
}

/// SOUNDNESS GUARD: see [`ORDER_SENSITIVE_AGGS`]. Bare-name matched for the same reason as
/// [`reject_nondeterministic`].
///
/// This fires wherever a call is lowered, not only in aggregate position, because the two positions
/// are wrong in different ways and neither is worth keeping: in aggregate position the bag would
/// determine the result, which is the unsound assumption, and in scalar position — where these
/// currently land, having matched no aggregate list — the cardinality is wrong outright.
fn reject_order_sensitive_agg(full: &str, bare: &str) -> Result<()> {
    if ORDER_SENSITIVE_AGGS.contains(&bare) {
        return Err(unsupported(format!("order-sensitive aggregate {full}")));
    }
    Ok(())
}

/// SOUNDNESS GUARD: see [`UNMODELLED_AGGS`]. Bare-name matched and called wherever a call is lowered,
/// as [`reject_order_sensitive_agg`] is.
fn reject_unmodelled_agg(full: &str, bare: &str) -> Result<()> {
    if UNMODELLED_AGGS.contains(&bare) {
        return Err(unsupported(format!("unmodelled aggregate {full}")));
    }
    Ok(())
}

/// SOUNDNESS GUARD: see [`FLOAT_SUMMING_AGGS`] and [`FLOAT_ONLY_AGGS`]. `args` are the lowered
/// arguments, whose types say whether one is a float -- a float column, or a value computed from one,
/// such as `coalesce(f, 0)` or `f * 2` ([`COARSE_OPAQUE`]).
fn reject_float_summing(name: &str, args: &[Value]) -> Result<()> {
    let float = args.iter().any(|v| coarse_class(&ty_of(v)) == Some("float"));
    if FLOAT_ONLY_AGGS.contains(&name) || (float && FLOAT_SUMMING_AGGS.contains(&name)) {
        return Err(unsupported(format!(
            "{name} adds in floating point, so its result depends on the order the rows arrive in"
        )));
    }
    Ok(())
}

/// Lower a top-level query to a `Relation` Value.
pub fn lower_query(cat: &Catalog, fns: &Fns, q: &Query) -> Result<Value> {
    Ok(lower_query_ctx(cat, fns, q, &[])?.0)
}

/// Lower a query in an enclosing context (`outer` = visible enclosing bindings, empty at top level),
/// returning the relation and its output columns. The output columns are needed when the query is a
/// derived table or subquery so the enclosing query can resolve its columns.
fn lower_query_ctx(cat: &Catalog, fns: &Fns, q: &Query, outer: &[Binding]) -> Result<(Value, OutCols)> {
    // `normalize::inline_ctes` replaces every binding it can with its definition and leaves the
    // `WITH` in place when one is recursive or writes. Nothing here reads a binding, so lowering past
    // it would resolve its name as whatever base table has that name, and drop its effect.
    if q.with.is_some() {
        return Err(unsupported("WITH clause that is not inlined (RECURSIVE, or a data-modifying binding)"));
    }
    // Every query node passes through here, so this is the one place a lock clause can be caught.
    // Identical ones were already dropped by `normalize::strip_identical_locks`; any left differ
    // between the sides, and the prover has no concurrency to tell them apart.
    if !q.locks.is_empty() {
        return Err(unsupported(
            "row-locking clause (FOR UPDATE / FOR SHARE) not identical on both sides",
        ));
    }
    let ord = OrderCtx::Known(q.order_by.as_ref());
    let (rel, out_cols, sortable) = lower_setexpr_ctx(cat, fns, q.body.as_ref(), outer, ord)?;
    let rel = apply_pagination(cat, fns, q, rel, &out_cols, sortable.as_ref())?;
    Ok((rel, out_cols))
}

/// Wrap a lowered body in the prover's `Sort` when the query takes a row slice.
///
/// `ORDER BY` on its own does not need a node: without a slice nothing downstream can observe the
/// order, and bag semantics make it immaterial, so it is dropped exactly as before. A slice is what
/// makes the order observable, and then the whole clause has to be carried.
///
/// # What `Sort` means to the prover, and why an opaque node still proves things
///
/// `Sort` evaluates to a chain of *uninterpreted* `HOp` applications (`relation.rs:436`) — one
/// `sort` per collation entry, bottoming out in `limit(count, offset, src)` or `offset(n, src)`.
/// Three cases are interpreted rather than opaque (`partial.rs:124`): `limit(0, _)` is the empty
/// bag, and `limit(1, ·)` over a degenerate source and `offset(0, ·)` are the identity.
///
/// Opaque does not mean inert. The memo key that mints the `HOp` symbol holds the
/// *SMT-canonicalized* source relation, so two sides whose bodies are equal modulo the solver —
/// commuted `AND`s, `a > 0` against `a >= 1`, commuted joins — land on the same symbol and the
/// wrapper is transparent to them. What congruence cannot see through is a difference in the
/// pagination *itself*; those pairs simply go unproved, which is the conservative direction.
///
/// # The collation index is a tuple position, not a level
///
/// Column references elsewhere in this module are absolute de-Bruijn levels (see [`crate::scope`]).
/// The collation index is not one: it is a 0-based position in the *source relation's output tuple*,
/// which here is `out_cols`. Confirmed against the prover's own fixtures — in
/// `tests/calcite/testSortProjectTranspose1.json` a `Sort` over a two-column `Project` carries
/// `collation: [[1, "INTEGER", "ASCENDING"]]`.
///
/// # Order is not canonicalized, deliberately
///
/// Evaluation pops the collation from the *end*, so entry order fixes the nesting of `sort` HOps and
/// therefore the symbol. Sorting or deduplicating the collation to make more pairs match would make
/// `ORDER BY a, b` congruent to `ORDER BY b, a`, which is a false-proof channel. The clause order is
/// preserved verbatim.
///
/// # An inherited assumption, stated because it is the one soft spot
///
/// Treating `sort` as a function of its source makes a slice deterministic, whereas real SQL leaves
/// the choice among tied rows — and all rows under a bare `LIMIT` with no `ORDER BY` — unspecified.
/// That is the prover's abstraction and the standard one, and it is the same assumption
/// [`crate::normalize::strip_identical_pagination`] already rests on; this function inherits it
/// rather than widening it.
fn apply_pagination(
    cat: &Catalog,
    fns: &Fns,
    q: &Query,
    rel: Value,
    out_cols: &OutCols,
    sortable: Option<&SortScope>,
) -> Result<Value> {
    let Some((limit, offset)) = row_slice(cat, fns, q)? else { return Ok(rel) };
    let collation = match collation(q, out_cols, sortable.is_none())? {
        CollationPlan::Direct(c) => c,
        // At least one key is an input expression, so the `Sort` has nothing to point at until it
        // is lowered in the select's FROM scope; only a select has one. See [`sort_sandwich`].
        CollationPlan::NeedsExtension => {
            let Some(sortable) = sortable else {
                return Err(unsupported("ORDER BY key is not an output column"));
            };
            return sort_sandwich(cat, fns, sortable, q, rel, out_cols, limit, offset);
        }
    };
    Ok(json!({
        "sort": { "collation": collation, "limit": limit, "offset": offset, "source": rel }
    }))
}

/// Calcite's `Project(trim) <- Sort <- Project(outputs ++ keys)`, for an `ORDER BY` over a value the
/// query does not output — `SELECT a FROM t ORDER BY b LIMIT 1`, or `ORDER BY lower(n)`.
///
/// The `Sort` collation is a position in its *source's* output tuple, so the only way to order by
/// something the projection dropped is to stop dropping it: lower the key in the select's own scope,
/// append it to the projection, point the collation at the new position, and trim it back off above
/// the `Sort` so the query's output is unchanged.
///
/// # Why this needs the select's scope, and why only a plain select is widened
///
/// The key is an arbitrary expression over the FROM bindings, which only [`lower_select_ctx`] has.
/// It passes its [`Scope`] up for exactly this ([`SortScope`]). Only a result that is a projection
/// directly over the FROM relation can be widened. A `DISTINCT` or a `DISTINCT ON` puts a `Group`
/// in between, and a FROM-scope expression cannot be addressed through one — appending it there
/// would silently order by whatever column happens to sit at that index — so there the key must
/// already be the value of an output column, which is Postgres's own rule for `SELECT DISTINCT`
/// (its `ORDER BY` expressions must appear in the select list), and is refused otherwise. A
/// `GROUP BY` gets no scope at all, nor do set operations and `VALUES`, which have no single FROM
/// scope to resolve against; those keep the refusal.
///
/// Keys are resolved by [`resolve_order_key`], so an output name or position here means what it
/// means in [`collation`], and ambiguity is still refused rather than resolved.
#[allow(clippy::too_many_arguments)]
fn sort_sandwich(
    cat: &Catalog,
    fns: &Fns,
    sortable: &SortScope,
    q: &Query,
    rel: Value,
    out_cols: &OutCols,
    limit: Option<Value>,
    offset: Option<Value>,
) -> Result<Value> {
    use sqlparser::ast::OrderByKind;

    let Some(order_by) = &q.order_by else {
        // `CollationPlan::NeedsExtension` is only reachable with keys to resolve.
        return Err(unsupported("ORDER BY key is not an output column"));
    };
    let OrderByKind::Expressions(keys) = &order_by.kind else {
        return Err(unsupported("ORDER BY ALL"));
    };

    let scope = &sortable.scope;
    let base = scope.base;
    let (mut targets, source) = match &sortable.outputs {
        Some(outputs) => (outputs.clone(), Value::Null),
        None => split_projection(&rel, out_cols, base),
    };
    let mut collation = Vec::with_capacity(keys.len());
    for key in keys {
        if key.with_fill.is_some() {
            return Err(unsupported("ORDER BY ... WITH FILL"));
        }
        let v = order_key_value(cat, scope, fns, &key.expr, &targets, out_cols, "ORDER BY")?;
        if sortable.outputs.is_some() && !targets.contains(&v) {
            return Err(unsupported("ORDER BY key is not an output column"));
        }
        let ty = ty_of(&v);
        let idx = push_unique(&mut targets, v);
        collation.push(json!([idx, ty, ord_string(&key.options)?]));
    }

    // Every key was already an output value (`ORDER BY t.a` over `SELECT t.a`): nothing was
    // appended, so the trim would be the identity and the `Sort` can stand on the body directly —
    // the shape [`collation`] gives the same ordering spelled as an output name or position.
    if targets.len() == out_cols.len() {
        return Ok(json!({
            "sort": { "collation": collation, "limit": limit, "offset": offset, "source": rel }
        }));
    }

    let extended = json!({ "project": { "target": targets, "source": source } });
    let sorted = json!({
        "sort": { "collation": collation, "limit": limit, "offset": offset, "source": extended }
    });
    // Trim the appended keys back off, so the query's output columns are the ones it declared.
    let trim: Vec<Value> = out_cols
        .iter()
        .enumerate()
        .map(|(i, (_, t))| json!({ "column": base + i, "type": t }))
        .collect();
    Ok(json!({ "project": { "target": trim, "source": sorted } }))
}

/// The `(limit, offset)` counts a query slices by, or `None` if it takes no slice.
///
/// `OFFSET` alone is a slice: it drops rows, so the order it drops them in is observable. `LIMIT`
/// alone is too, with no offset.
fn row_slice(
    cat: &Catalog,
    fns: &Fns,
    q: &Query,
) -> Result<Option<(Option<Value>, Option<Value>)>> {
    use sqlparser::ast::LimitClause;

    let (mut limit, mut offset) = (None, None);
    match &q.limit_clause {
        None => {}
        Some(LimitClause::LimitOffset { limit: l, offset: o, limit_by }) => {
            // ClickHouse `LIMIT n BY expr` keeps n rows *per group* — a different operator.
            if !limit_by.is_empty() {
                return Err(unsupported("LIMIT ... BY"));
            }
            limit = l.as_ref().map(|l| count(cat, fns, l)).transpose()?;
            offset = o.as_ref().map(|o| count(cat, fns, &o.value)).transpose()?;
        }
        // MySQL `LIMIT offset, limit`.
        Some(LimitClause::OffsetCommaLimit { offset: o, limit: l }) => {
            limit = Some(count(cat, fns, l)?);
            offset = Some(count(cat, fns, o)?);
        }
    }

    if let Some(f) = &q.fetch {
        // `WITH TIES` returns a variable number of rows, so the count is not the row count.
        if f.with_ties {
            return Err(unsupported("FETCH ... WITH TIES"));
        }
        if f.percent {
            return Err(unsupported("FETCH ... PERCENT"));
        }
        // `OFFSET n ROWS FETCH NEXT m ROWS ONLY` splits across both fields, so a FETCH alongside an
        // OFFSET-only LIMIT clause is standard. A FETCH alongside an actual LIMIT is contradictory.
        if limit.is_some() {
            return Err(unsupported("both LIMIT and FETCH"));
        }
        // Bare `FETCH FIRST ROW ONLY` means one row.
        limit = Some(match &f.quantity {
            Some(e) => count(cat, fns, e)?,
            None => json!({ "operator": "1", "operand": [], "type": "INTEGER" }),
        });
    }

    Ok((limit.is_some() || offset.is_some()).then_some((limit, offset)))
}

/// A `LIMIT`/`OFFSET` count, lowered as an ordinary expression against an empty scope.
///
/// The prover evaluates the count with `env.eval` like any other expression (`relation.rs:445`), so
/// it need not be a literal — and it must not be restricted to one, because the overwhelmingly
/// common shape in real query logs is `LIMIT $3`, which [`crate::casts::substitute_params`] has
/// already rewritten to the nullary constant `qp3(0)` by the time lowering runs.
///
/// The scope is empty on purpose. A count cannot reference a column, so there is nothing to resolve
/// against; passing the body's scope would silently accept `LIMIT a` as a column reference.
///
/// Only the interpreted cases care what the count *is*: `partial.rs:124` compares the evaluated
/// argument structurally against literal `0` and `1`, so `qp3(0)` matches neither and the `limit`
/// `HOp` stays opaque. Two sides sharing a parameter share its symbol, so `LIMIT $3` proves against
/// `LIMIT $3`; `LIMIT $3` against `LIMIT $4` is two different symbols and simply goes unproved.
/// The count must come out `INTEGER`. `infer`'s pass 7b normally sees to that, but a parameter with
/// competing `Conf::Cast` evidence can still land elsewhere, and a count on any other sort is not a
/// weaker proof — it is a `SortDiffers` panic inside z3 the moment congruence asserts the two sides'
/// counts equal (the prover's absent-offset default is an `Int` literal, so even one side suffices).
/// Refusing is the conservative direction and keeps a frontend gap from presenting as a prover crash.
fn count(cat: &Catalog, fns: &Fns, e: &Expr) -> Result<Value> {
    let v = lower_expr(cat, &Scope::empty(), fns, e)?;
    match ty_of(&v).as_str() {
        "INTEGER" => Ok(v),
        other => Err(unsupported(format!("LIMIT/OFFSET count typed {other}, not INTEGER"))),
    }
}

/// The collation for a sliced query: one entry per `ORDER BY` key, in clause order.
///
/// Empty is legal and meaningful — `LIMIT 5` with no `ORDER BY` lowers to a bare `limit` HOp.
///
/// An input-expression key asks for [`sort_sandwich`], which lowers it in the select's FROM scope.
/// Only when there is no such scope to go to (`by_tree`: a grouped select) is the key matched here,
/// against a select-list item written the same way ([`projected_as`]).
fn collation(q: &Query, out_cols: &OutCols, by_tree: bool) -> Result<CollationPlan> {
    use sqlparser::ast::OrderByKind;

    let Some(order_by) = &q.order_by else { return Ok(CollationPlan::Direct(Vec::new())) };
    let keys = match &order_by.kind {
        OrderByKind::Expressions(keys) => keys,
        // `ORDER BY ALL` orders by every output column; the expansion is not worth guessing.
        OrderByKind::All(_) => return Err(unsupported("ORDER BY ALL")),
    };

    let mut out = Vec::with_capacity(keys.len());
    for key in keys {
        if key.with_fill.is_some() {
            return Err(unsupported("ORDER BY ... WITH FILL"));
        }
        let idx = match resolve_order_key(&key.expr, out_cols, "ORDER BY")? {
            OrderKey::Output(i) => i,
            OrderKey::Input(e) => match by_tree.then(|| projected_as(q, e)).flatten() {
                Some(i) => i,
                None => return Ok(CollationPlan::NeedsExtension),
            },
        };
        out.push(json!([idx, out_cols[idx].1, ord_string(&key.options)?]));
    }
    Ok(CollationPlan::Direct(out))
}

/// Whether a query's `ORDER BY` can be pointed straight at its output columns.
enum CollationPlan {
    /// Every key is an output column; the `Sort` stacks directly on the body.
    Direct(Vec<Value>),
    /// A key is an expression, or a column the projection drops. [`sort_sandwich`] widens the
    /// projection so the collation has something to address.
    NeedsExtension,
}

/// What one `ORDER BY` or `DISTINCT ON` key refers to, as Postgres reads it.
enum OrderKey<'e> {
    /// The query's own output column at this (0-based) position.
    Output(usize),
    /// An expression over the FROM clause, to be lowered in its scope.
    Input(&'e Expr),
}

/// Resolve one `ORDER BY` or `DISTINCT ON` key by Postgres's rules (`findTargetlistEntrySQL92`,
/// which `DISTINCT ON` shares with `ORDER BY`), in this order:
///
/// 1. an integer is a 1-based position in the select list;
/// 2. a *bare* name is the output column of that name, if there is one;
/// 3. anything else — a qualified name like `t.a` included, always — is an expression over the
///    FROM clause.
///
/// The parser drops parentheses before Postgres looks, so `(1)` is still a position and `(a)` still
/// a bare name. Names compare as Postgres folds them (see [`fold_name`]): `A` is `a`, and neither is
/// the output column `"A"`.
///
/// Every caller shares this, and what each does with [`OrderKey::Input`] is its own business:
/// [`sort_sandwich`] and [`distinct_on`] lower it in the FROM scope, while [`collation`], which has
/// no FROM scope, can only use a select-list item written the same way ([`projected_as`]), and
/// does so only for a grouped select, which passes no scope up.
///
/// Refused rather than resolved, because there is no answer that is right whichever way the
/// query meant it:
///
/// - a bare name that two output columns carry (Postgres accepts it only when the two are the
///   same expression, and raises otherwise);
/// - a bare name that matches no output column whose name is known, when one of the output columns
///   is an expression whose Postgres name this frontend cannot tell ([`expr_name`]): Postgres would
///   read the key as that column if the name were the same, and as an input column otherwise.
///
/// A wildcard needs no special case: `expand_projection` has already resolved `*` into named
/// columns by the time `out_cols` exists, so `SELECT * FROM t ORDER BY b` resolves like any other.
fn resolve_order_key<'e>(e: &'e Expr, out_cols: &OutCols, clause: &str) -> Result<OrderKey<'e>> {
    match crate::casts::unwrap_nested(e) {
        Expr::Value(v) => {
            if let SqlValue::Number(n, _) = &v.value {
                let pos: usize = n.parse().map_err(|_| unsupported(format!("{clause} position {n}")))?;
                return match pos.checked_sub(1).filter(|&i| i < out_cols.len()) {
                    Some(i) => Ok(OrderKey::Output(i)),
                    None => Err(schema(format!("{clause} position {pos} out of range"))),
                };
            }
        }
        Expr::Identifier(id) => {
            let name = fold_name(id);
            let mut found =
                out_cols.iter().enumerate().filter(|(_, (n, _))| !is_unnamed(n) && *n == name);
            match (found.next(), found.next()) {
                (Some((i, _)), None) => return Ok(OrderKey::Output(i)),
                // Which one the key means is a resolution question we decline rather than answer
                // arbitrarily. Widening the projection would not help: the ambiguity is in the name.
                (Some(_), Some(_)) => return Err(schema(format!("ambiguous {clause} key {name}"))),
                (None, _) => {}
            }
            if out_cols.iter().any(|(n, _)| is_unnamed(n)) {
                return Err(unsupported(format!(
                    "{clause} key {name} beside an output column whose name is not known"
                )));
            }
        }
        _ => {}
    }
    Ok(OrderKey::Input(e))
}

/// The output position of a select-list item written exactly as the key `e`, if there is one.
///
/// For a key that is not an output name, Postgres lowers it in the FROM scope and then looks for a
/// select-list item that is the same expression (`findTargetlistEntrySQL99`). This is that rule
/// cut down to what can be checked without lowering: the same tree, in the same select, over the
/// same scope, is the same expression. Postgres also matches trees that differ only in spelling
/// (`t.a` against `a`); those find nothing here and are refused a level up, which is the
/// conservative side.
///
/// A wildcard item makes the item index stop being the output position, so a select carrying one
/// matches nothing.
fn projected_as(q: &Query, e: &Expr) -> Option<usize> {
    use crate::casts::unwrap_nested;
    let SetExpr::Select(s) = q.body.as_ref() else { return None };
    if s.projection.iter().any(|it| item_expr(it).is_none()) {
        return None;
    }
    let e = unwrap_nested(e);
    s.projection.iter().position(|it| item_expr(it).is_some_and(|x| unwrap_nested(x) == e))
}

/// The direction tag for one collation entry.
///
/// Both defaults are resolved rather than passed through, so that `ORDER BY a` and
/// `ORDER BY a ASC NULLS LAST` — the same ordering, spelled two ways — produce the same tag and can
/// be proved equal.
///
/// Null placement is folded in because the collation tuple has nowhere else to put it: its three
/// fields are index, type, and this string. Leaving it out would make `NULLS FIRST` and `NULLS LAST`
/// mint the same symbol and prove equal, which is a false-proof channel. Postgres defaults are
/// `NULLS LAST` for ascending and `NULLS FIRST` for descending.
///
/// `USING <operator>` is refused: its direction is whatever the operator's btree class says, and
/// reading it as the ascending default would let `USING >` prove equal to `ASC`.
fn ord_string(o: &sqlparser::ast::OrderByOptions) -> Result<String> {
    use sqlparser::ast::OrderBySort;
    let desc = match &o.sort {
        None | Some(OrderBySort::Asc) => false,
        Some(OrderBySort::Desc) => true,
        Some(OrderBySort::Using(_)) => return Err(unsupported("ORDER BY ... USING <operator>")),
    };
    let dir = if desc { "DESCENDING" } else { "ASCENDING" };
    let nulls_first = o.nulls_first.unwrap_or(desc);
    Ok(format!("{dir} NULLS {}", if nulls_first { "FIRST" } else { "LAST" }))
}

/// The enclosing query's `ORDER BY`, threaded down for the one construct whose meaning depends on
/// it: `DISTINCT ON` (see [`distinct_on`]).
///
/// Nothing else in this module reads the clause — [`apply_pagination`] drops it when the query
/// takes no row slice, because without a slice bag semantics make the order unobservable. For
/// `DISTINCT ON` it is observable: the clause is what picks the surviving row. The clause hangs off
/// the enclosing [`Query`] while `DISTINCT ON` hangs off the [`Select`], so it has to be carried.
///
/// `Unknown` is deliberately distinct from `Known(None)`. A `SELECT` reached through a set
/// operation is governed by an `ORDER BY` that belongs to the set operation rather than to it, and
/// reading that as "no ORDER BY" would hand two differently-ordered `DISTINCT ON`s the same symbol
/// — a false-proof channel. `Unknown` refuses instead of guessing.
#[derive(Clone, Copy)]
enum OrderCtx<'a> {
    Known(Option<&'a OrderBy>),
    Unknown,
}

/// What an enclosing `Sort` can do with an `ORDER BY` key that is an input expression rather than
/// an output column: the select's own FROM scope to lower it in, and, where the select's result
/// cannot be widened, the values its output columns hold over that scope. See [`sort_sandwich`].
struct SortScope {
    scope: Scope,
    /// `None` for a plain select, whose projection sits directly on the FROM relation and is
    /// widened with the key. `Some` for a `DISTINCT` or `DISTINCT ON` select, whose key has to be
    /// one of these values already.
    outputs: Option<Vec<Value>>,
}

/// Lower a set-expression body: a plain SELECT, a set operation, a parenthesized query, or VALUES.
fn lower_setexpr_ctx(
    cat: &Catalog,
    fns: &Fns,
    body: &SetExpr,
    outer: &[Binding],
    ord: OrderCtx,
) -> Result<(Value, OutCols, Option<SortScope>)> {
    match body {
        SetExpr::Select(s) => lower_select_ctx(cat, fns, s, outer, ord),
        // A parenthesized query carries its own ORDER BY; `ord` belongs to the enclosing one, and
        // its own `Sort` (if any) is already in place, so there is nothing left to widen.
        SetExpr::Query(q) => lower_query_ctx(cat, fns, q, outer).map(|(v, c)| (v, c, None)),
        SetExpr::SetOperation { op, set_quantifier, left, right } => {
            let (lv, lcols, _) = lower_setexpr_ctx(cat, fns, left, outer, OrderCtx::Unknown)?;
            let (rv, rcols, _) = lower_setexpr_ctx(cat, fns, right, outer, OrderCtx::Unknown)?;
            // Postgres resolves each output column to one type across both branches, promoting a
            // DATE branch against a TIMESTAMP one. The prover takes the columns as they stand, so a
            // branch pair that differs across a temporal boundary would put two units in one column;
            // with no conversion to insert inside a branch from here, it is refused.
            if let Some(((_, a), (_, b))) =
                lcols.iter().zip(&rcols).find(|((_, a), (_, b))| temporal_mismatch(a, b) || hides_coarse(a, b))
            {
                return Err(unsupported(format!("set operation over columns of type {a} and {b}")));
            }
            let all = matches!(set_quantifier, SetQuantifier::All | SetQuantifier::AllByName);
            let rel = match op {
                SetOperator::Union => {
                    let u = json!({ "union": [lv, rv] });
                    if all { u } else { json!({ "distinct": u }) }
                }
                // The prover's Intersect/Except are set-semantics (squash); ALL = bag => refuse.
                SetOperator::Intersect if all => return Err(unsupported("INTERSECT ALL (bag semantics)")),
                SetOperator::Intersect => json!({ "intersect": [lv, rv] }),
                // MINUS is a non-standard synonym for EXCEPT.
                SetOperator::Except | SetOperator::Minus if all => {
                    return Err(unsupported("EXCEPT/MINUS ALL (bag semantics)"))
                }
                SetOperator::Except | SetOperator::Minus => json!({ "except": [lv, rv] }),
            };
            Ok((rel, lcols, None))
        }
        SetExpr::Values(v) => lower_values(cat, fns, v).map(|(r, c)| (r, c, None)),
        other => Err(unsupported(format!("query body {other:?}"))),
    }
}

/// `VALUES (...), (...)` -> a prover `Values` relation.
fn lower_values(cat: &Catalog, fns: &Fns, v: &Values) -> Result<(Value, OutCols)> {
    if v.rows.is_empty() {
        return Err(unsupported("empty VALUES"));
    }
    let empty = Scope::empty();
    let content: Vec<Vec<Value>> = v
        .rows
        .iter()
        .map(|row| row.iter().map(|e| lower_expr(cat, &empty, fns, e)).collect::<Result<Vec<_>>>())
        .collect::<Result<Vec<_>>>()?;
    let schema_tys: Vec<String> = content[0].iter().map(ty_of).collect();
    // Same reason as the set operations: the column type is the first row's, and a later row of
    // another temporal type would hold a value in another unit, or one whose `=` is not identity a
    // value the column's type does not say that of.
    for row in &content[1..] {
        if let Some((a, b)) = schema_tys
            .iter()
            .zip(row.iter().map(ty_of))
            .find(|(a, b)| temporal_mismatch(a, b) || hides_coarse(a, b))
        {
            return Err(unsupported(format!("VALUES column of type {a} holding a {b}")));
        }
    }
    let out_cols: OutCols =
        schema_tys.iter().enumerate().map(|(i, t)| (unnamed(i), t.clone())).collect();
    Ok((json!({ "values": { "schema": schema_tys, "content": content } }), out_cols))
}

/// Whether `e` is *closed*: no column reference and no nested query anywhere inside it.
///
/// The walk is the derived AST visitor rather than a hand-written match on purpose. Every caller
/// uses this as a soundness guard, so the failure mode that matters is missing a variant — and a
/// hand-written match silently misses every variant added by a parser upgrade.
pub(crate) fn is_closed(e: &Expr) -> bool {
    visit_expressions(e, |x| match x {
        Expr::Identifier(_)
        | Expr::CompoundIdentifier(_)
        | Expr::CompoundFieldAccess { .. }
        | Expr::Subquery(_)
        | Expr::Exists { .. }
        | Expr::InSubquery { .. }
        | Expr::AnyOp { .. }
        | Expr::AllOp { .. } => ControlFlow::Break(()),
        _ => ControlFlow::Continue(()),
    })
    .is_continue()
}

/// A `FROM`-less `SELECT e1, ..., en`: one row, no input relation.
///
/// When every expression is closed it is `VALUES (e1, ..., en)`, which is how it is emitted. The
/// prover evaluates a `Values` row's content one level *above* the row itself
/// (`Env(.., lvl + scope.len())`), so a correlated column or a nested subquery there would have to
/// be numbered differently than everywhere else in the frontend. Such a projection goes over the
/// prover's one-row, zero-column `singleton` instead, where the targets see the enclosing row and
/// nothing else -- the scope a `FROM` with no items would have.
fn lower_fromless_select(cat: &Catalog, fns: &Fns, s: &Select, outer: &[Binding]) -> Result<(Value, OutCols)> {
    // Every remaining clause needs a source row to mean anything; none of them are degenerate
    // enough to just drop, so a FROM-less SELECT carrying one is refused.
    if s.selection.is_some() || s.having.is_some() || !group_by_empty(s) || s.distinct.is_some() {
        return Err(unsupported("FROM-less SELECT with WHERE/GROUP BY/HAVING/DISTINCT"));
    }
    let mut items: Vec<(&Expr, String)> = Vec::new();
    for (idx, item) in s.projection.iter().enumerate() {
        let (e, name) = match item {
            SelectItem::UnnamedExpr(e) => (e, expr_name(e, idx)),
            SelectItem::ExprWithAlias { expr, alias } => (expr, fold_name(alias)),
            // A wildcard needs a FROM to expand against, so this is not valid SQL to begin with.
            other => return Err(unsupported(format!("FROM-less SELECT projection {other:?}"))),
        };
        // An aggregate here has no rows to fold over; `contains_agg` keeps it out of both shapes,
        // where it would otherwise be emitted as if it were a scalar.
        if contains_agg(fns, e) {
            return Err(unsupported("aggregate in a FROM-less SELECT"));
        }
        items.push((e, name));
    }
    if items.is_empty() {
        return Err(unsupported("FROM-less SELECT with no projection"));
    }
    if items.iter().all(|(e, _)| is_closed(e)) {
        let empty = Scope::empty();
        let content = items.iter().map(|(e, _)| lower_expr(cat, &empty, fns, e)).collect::<Result<Vec<_>>>()?;
        let out_cols: OutCols = items.iter().zip(&content).map(|((_, n), v)| (n.clone(), ty_of(v))).collect();
        let schema: Vec<String> = content.iter().map(ty_of).collect();
        return Ok((json!({ "values": { "schema": schema, "content": [content] } }), out_cols));
    }
    let scope = Scope {
        binds: outer.to_vec(),
        inner_count: 0,
        base: Scope::outer_width(outer),
        merged: Vec::new(),
        merged_outer: false,
        coalesced: Vec::new(),
    };
    let targets = items.iter().map(|(e, _)| lower_expr(cat, &scope, fns, e)).collect::<Result<Vec<_>>>()?;
    let out_cols: OutCols = items.iter().zip(&targets).map(|((_, n), v)| (n.clone(), ty_of(v))).collect();
    Ok((json!({ "project": { "target": targets, "source": "singleton" } }), out_cols))
}

fn lower_select_ctx(
    cat: &Catalog,
    fns: &Fns,
    s: &Select,
    outer: &[Binding],
    ord: OrderCtx,
) -> Result<(Value, OutCols, Option<SortScope>)> {
    // SOUNDNESS GUARDS: refuse result-changing clauses we don't faithfully lower.
    if s.top.is_some() {
        return Err(unsupported("TOP"));
    }
    if !s.sort_by.is_empty() || !s.cluster_by.is_empty() || !s.distribute_by.is_empty() {
        return Err(unsupported("SORT/CLUSTER/DISTRIBUTE BY"));
    }
    if s.qualify.is_some() {
        return Err(unsupported("QUALIFY"));
    }
    // `SELECT ... INTO t` is `CREATE TABLE t AS SELECT ...`: it returns no rows and creates a table.
    if s.into.is_some() {
        return Err(unsupported("SELECT ... INTO"));
    }
    if s.from.is_empty() {
        let (rel, cols) = lower_fromless_select(cat, fns, s, outer)?;
        return Ok((rel, cols, None));
    }

    let (scope, mut rel) = build_from_clause(cat, fns, &s.from, outer)?;

    if let Some(w) = &s.selection {
        let cond = lower_bool(cat, &scope, fns, w)?;
        rel = json!({ "filter": { "condition": cond, "source": rel } });
    }

    let has_agg = s.projection.iter().any(|it| item_expr(it).is_some_and(|e| contains_agg(fns, e)))
        || s.having.as_ref().is_some_and(|h| contains_agg(fns, h));
    let aggregated = has_agg || !group_by_empty(s) || s.having.is_some();
    let (result, out_cols) = if aggregated {
        lower_aggregate(cat, &scope, fns, rel, s)?
    } else if is_pure_wildcard(s) && !scope.hides_columns() && scope.merged.is_empty() {
        // `SELECT *` over the FROM relation *is* that relation — but only while every one of its
        // columns is visible. A system column is not, so taking the shortcut there would hand the
        // caller a relation one column wider than the shape it was just told the query has. Nor is
        // it once a `USING` has merged two columns into one: `expand_projection` refuses that `*`.
        (rel, scope.out_cols())
    } else {
        let proj = expand_projection(cat, &scope, fns, s)?;
        let targets: Vec<Value> = proj.iter().map(|(_, v)| v.clone()).collect();
        let cols: OutCols = proj.iter().map(|(n, v)| (n.clone(), ty_of(v))).collect();
        (json!({ "project": { "target": targets, "source": rel } }), cols)
    };

    // Only a projection sitting directly on the FROM relation can be widened with an ORDER BY key;
    // above a `DISTINCT`'s `Group` the key can only be an output value, and an aggregate's output
    // values are not over the FROM scope at all. See [`sort_sandwich`].
    let outputs = match &s.distinct {
        _ if aggregated => None,
        None | Some(Distinct::All) => Some(None),
        Some(_) => Some(Some(split_projection(&result, &out_cols, scope.base).0)),
    };
    let (rel, cols) = apply_distinct(cat, fns, &scope, s, result, out_cols, ord, aggregated)?;
    Ok((rel, cols, outputs.map(|outputs| SortScope { scope, outputs })))
}

/// `SELECT DISTINCT` -> Group keyed on all output columns (matches Calcite's canonical form, where
/// `DISTINCT` and a no-aggregate `GROUP BY` become the same Aggregate node). `DISTINCT ON` goes to
/// [`distinct_on`].
///
/// The keys index the projection underneath, which is a binder of our own making — hence
/// [`Scope::base`], not a plain `0..n`.
#[allow(clippy::too_many_arguments)]
fn apply_distinct(
    cat: &Catalog,
    fns: &Fns,
    scope: &Scope,
    s: &Select,
    result: Value,
    out_cols: OutCols,
    ord: OrderCtx,
    aggregated: bool,
) -> Result<(Value, OutCols)> {
    let base = scope.base;
    match &s.distinct {
        // `SELECT ALL` keeps duplicates -> same as no DISTINCT.
        None | Some(Distinct::All) => Ok((result, out_cols)),
        Some(Distinct::Distinct) => {
            let keys: Vec<Value> = out_cols
                .iter()
                .enumerate()
                .map(|(i, (_, t))| json!({ "column": base + i, "type": t }))
                .collect();
            Ok((json!({ "group": { "keys": keys, "function": [], "source": result } }), out_cols))
        }
        Some(Distinct::On(keys)) => {
            distinct_on(cat, fns, scope, keys, result, out_cols, ord, aggregated)
        }
    }
}

/// `SELECT DISTINCT ON (k…) c₁…cₙ FROM src ORDER BY …` -> a `Group` on `k…` whose columns are
/// *uninterpreted* aggregates, trimmed back to `c₁…cₙ`.
///
/// # Why it cannot be given exact semantics
///
/// `DISTINCT ON` keeps one row per distinct key — *the first one under the query's `ORDER BY`*. It is
/// order-sensitive, and the prover's relational algebra is bag-semantic, so there is no faithful
/// encoding. That is the bind `Sort` is already in, and the answer is the same one
/// [`apply_pagination`] uses: emit an opaque operator and let the prover's memo key decide when two
/// of them are the same.
///
/// # The encoding, and why it needs no new prover node
///
/// The prover's `Relation` enum has no generic opaque-relation node, but it does have an opaque
/// *aggregate*: `relation.rs:417` falls through to `HOp(op, [], Lambda(inner_scope, body), ty)` for
/// any aggregate op it does not recognise, where `body` sums over the tuples of the group. So an
/// `AggCall` with an invented name already **is** an uninterpreted higher-order term over the group's
/// multiset of rows — exactly the thing "pick one row of this group" needs.
///
/// The shape mirrors [`lower_aggregate`], as a sandwich:
///
/// ```text
/// Project(trim) <- Group{ keys, one AggCall per output column } <- Project(outputs ++ order keys)
/// ```
///
/// The bottom projection is the one non-obvious part and it is load-bearing; see *obligation 1*. The
/// top one trims because the prover's `Group` yields `keys ++ columns` while `DISTINCT ON` yields
/// only the columns.
///
/// Three properties of that fallthrough are easy to lose:
///
/// - **`ignoreNulls` must be `false`.** It defaults to `true` in the prover, which wraps the body in
///   `[not null]` predicates on the inner variables — that would silently drop every candidate row
///   with a NULL in any column, and `DISTINCT ON` drops nothing.
/// - **`args` must have length ≥ 2**, or the single-argument branch just above the fallthrough emits
///   an *interpreted* `Aggr` instead. Passing the whole extended tuple all but guarantees it, and
///   [`distinct_on_args`] pads the one degenerate case.
/// - **Do not reuse `Sort`.** A `Sort` with no row slice bottoms out in `HOp("offset", [0], src)`,
///   and `offset(0, ·)` is *interpreted as the identity* in `partial.rs` — so a `Sort`-based encoding
///   would silently equate `DISTINCT ON (k) …` with the plain `SELECT`.
///
/// `Group` with a non-empty key list takes the `squash(sum(…))` branch at `relation.rs:427`, so the
/// empty-keys scalar-aggregation guard is not in play; `DISTINCT ON` always has at least one key.
///
/// # Soundness obligation 1: the ordering values must be *in* the term
///
/// Two sides collapse onto one term exactly when the op name, the arguments, the keys and the source
/// all agree — the last three structurally, and modulo everything the solver already knows about the
/// source. Everything that distinguishes one `DISTINCT ON` from another therefore has to be visible
/// in one of those four places, or two *different* operators compare equal.
///
/// The trap is that the `ORDER BY` is visible in none of them. A query taking no row slice never
/// builds a `Sort`, so the clause reaches this function and nowhere else. Naming it in the op name is
/// **not** enough, because identical clause text can resolve to different values:
///
/// ```text
/// A: SELECT DISTINCT ON (k) k, v FROM ev                            ORDER BY k, t DESC
/// B: SELECT DISTINCT ON (k) k, v FROM (SELECT k, v, -t AS t FROM ev) ORDER BY k, t DESC
/// ```
///
/// Both print `ORDER BY k, t DESC`, both project `(k, v)`, and both sit on a projection that has
/// already dropped `t` — so with only the outputs as arguments the two terms are identical, while A
/// takes the largest `t` of each group and B the smallest. That pair proves equal, wrongly.
///
/// The fix is the bottom projection: the `ORDER BY` key expressions are lowered, appended to it, and
/// passed as arguments alongside the outputs. The lambda then binds the ordering values, `-t` and `t`
/// are structurally different sources, and the two sides no longer unify. The op name is left holding
/// only what is genuinely not a value — for each key, *which argument* it is and in *which direction*
/// it sorts (see [`order_digest`]). That digest is built from resolved positions and canonicalized
/// directions rather than from clause text, so `ORDER BY t` and `ORDER BY ev.t ASC NULLS LAST` are the
/// same operator, as they should be.
///
/// With that in place the argument closes: if two `DISTINCT ON`s land on one term then their extended
/// projections are structurally equal — same outputs, same ordering values, in the same positions —
/// their key lists are equal, their sources are equal to the solver, and the digest pins the same
/// argument positions to the same directions. They are the same operator.
///
/// # Soundness obligation 2: per-column aggregates over-approximate, safely
///
/// Modelling each output column as its own uninterpreted aggregate lets a model draw column 1 from
/// one row of the group and column 2 from another — something the real operator never does. That
/// admits *more* behaviours than SQL, so it can only ever cause a failure to prove, never a false
/// proof. A pair proved under it is equal for the real operator too.
///
/// # `DISTINCT ON` with no `ORDER BY`
///
/// Postgres then picks an arbitrary row. Two such queries land on the same symbol and can be proved
/// equal, which reads the nondeterminism as "the same implementation makes the same choice on equal
/// inputs" — precisely the assumption `HOp("limit", …)` already makes for `LIMIT` without `ORDER BY`
/// (see [`apply_pagination`]). This inherits that convention rather than widening it.
#[allow(clippy::too_many_arguments)]
fn distinct_on(
    cat: &Catalog,
    fns: &Fns,
    scope: &Scope,
    keys: &[Expr],
    result: Value,
    out_cols: OutCols,
    ord: OrderCtx,
    aggregated: bool,
) -> Result<(Value, OutCols)> {
    // `DISTINCT ON` over a grouped query resolves its keys against the *post-aggregation* output,
    // not the FROM scope, so the key expressions lowered here would address the wrong tuple. Rare
    // enough not to be worth a second resolution path.
    if aggregated {
        return Err(unsupported("DISTINCT ON over an aggregate query"));
    }
    if keys.is_empty() {
        return Err(unsupported("DISTINCT ON with no keys"));
    }
    // Not the same as "no ORDER BY": see [`OrderCtx`]. Guessing here is a false-proof channel.
    let OrderCtx::Known(order_by) = ord else {
        return Err(unsupported("DISTINCT ON under a set operation"));
    };

    let base = scope.base;
    let (mut targets, source) = split_projection(&result, &out_cols, base);

    // Extend the projection with anything the operator depends on that the outputs do not already
    // carry: the DISTINCT ON keys, then the ORDER BY keys. `push_unique` reuses an existing column
    // when the lowered expression is already there, so the common case appends nothing.
    //
    // A key resolves as an `ORDER BY` key does (see [`resolve_order_key`]): `DISTINCT ON (1)` is
    // the first output column, not the constant 1, and a bare name is an output name first.
    let mut gkeys = Vec::with_capacity(keys.len());
    for k in keys {
        let v = order_key_value(cat, scope, fns, k, &targets, &out_cols, "DISTINCT ON")?;
        let ty = ty_of(&v);
        let idx = push_unique(&mut targets, v);
        gkeys.push(json!({ "column": base + idx, "type": ty }));
    }
    let digest = match order_by {
        Some(o) => order_digest(cat, scope, fns, o, &mut targets, &out_cols)?,
        None => String::new(),
    };

    let args = distinct_on_args(&targets, base);
    let funcs: Vec<Value> = out_cols
        .iter()
        .enumerate()
        .map(|(i, (_, t))| {
            json!({
                "operator": format!("DISTINCT_ON#{i}<{digest}>"),
                "operand": args,
                "type": t,
                "distinct": false,
                // See the soundness notes: the default is `true`, which would drop candidate rows.
                "ignoreNulls": false,
            })
        })
        .collect();

    let m = gkeys.len();
    let inner = json!({ "project": { "target": targets, "source": source } });
    let group = json!({ "group": { "keys": gkeys, "function": funcs, "source": inner } });
    // `Group` yields `keys ++ columns`; `DISTINCT ON` yields only the columns.
    let trim: Vec<Value> = out_cols
        .iter()
        .enumerate()
        .map(|(i, (_, t))| json!({ "column": base + m + i, "type": t }))
        .collect();
    Ok((json!({ "project": { "target": trim, "source": group } }), out_cols))
}

/// The argument list every `DISTINCT ON` aggregate is a lambda over: the whole extended tuple, so the
/// term ranges over exactly the candidate rows *and* the values that choose between them.
///
/// A one-column tuple is padded with a repeat of itself, because a single-argument `AggCall` whose
/// argument type equals its result type takes the *interpreted* `Aggr` branch at `relation.rs:414`
/// instead of the opaque fallthrough. The duplicate is inert — it adds a second bound variable
/// constrained to the same value — and it keeps the operator uninterpreted. Only a one-column
/// `DISTINCT ON` on its own output column with no `ORDER BY` gets that far.
fn distinct_on_args(targets: &[Value], base: usize) -> Vec<Value> {
    let mut args: Vec<Value> = targets
        .iter()
        .enumerate()
        .map(|(i, v)| json!({ "column": base + i, "type": ty_of(v) }))
        .collect();
    if args.len() == 1 {
        args.push(args[0].clone());
    }
    args
}

/// A relation's output expressions and the relation they are computed over.
///
/// A `Project` splits into exactly that. The only other relation reaching here is a bare `SELECT *`
/// body, which exposes its source columns positionally — the identity list over itself. (A `Group`
/// cannot reach here: [`distinct_on`] refuses aggregated queries first.)
fn split_projection(result: &Value, out_cols: &OutCols, base: usize) -> (Vec<Value>, Value) {
    let proj = result.get("project");
    let target = proj.and_then(|p| p.get("target")).and_then(|t| t.as_array());
    let source = proj.and_then(|p| p.get("source"));
    if let (Some(t), Some(s)) = (target, source) {
        return (t.clone(), s.clone());
    }
    let identity = out_cols
        .iter()
        .enumerate()
        .map(|(i, (_, t))| json!({ "column": base + i, "type": t }))
        .collect();
    (identity, result.clone())
}

/// Position of `v` in `targets`, appending it first if it is not already there.
fn push_unique(targets: &mut Vec<Value>, v: Value) -> usize {
    match targets.iter().position(|t| *t == v) {
        Some(i) => i,
        None => {
            targets.push(v);
            targets.len() - 1
        }
    }
}

/// A canonical string for an `ORDER BY` clause, naming the *argument positions* it orders by rather
/// than the text it was written as. Extends `targets` with any key it has to add.
///
/// One `pos:direction;` entry per key, in clause order. Positions are indices into the extended
/// projection, which both sides of a pair must share structurally for their terms to unify at all, so
/// they mean the same thing on both sides. Directions go through [`ord_string`], which resolves the
/// `ASC`/`NULLS` defaults, so `ORDER BY a` and `ORDER BY a ASC NULLS LAST` — one ordering spelled two
/// ways — digest identically and can be proved equal.
///
/// Clause forms whose effect is not captured by "value, direction, null placement" are refused rather
/// than digested to something that ignores them.
fn order_digest(
    cat: &Catalog,
    scope: &Scope,
    fns: &Fns,
    ord: &OrderBy,
    targets: &mut Vec<Value>,
    out_cols: &OutCols,
) -> Result<String> {
    use sqlparser::ast::OrderByKind;

    if ord.interpolate.is_some() {
        return Err(unsupported("ORDER BY ... INTERPOLATE"));
    }
    let keys = match &ord.kind {
        OrderByKind::Expressions(keys) => keys,
        OrderByKind::All(_) => return Err(unsupported("ORDER BY ALL")),
    };
    let mut out = String::new();
    for key in keys {
        if key.with_fill.is_some() {
            return Err(unsupported("ORDER BY ... WITH FILL"));
        }
        let v = order_key_value(cat, scope, fns, &key.expr, targets, out_cols, "ORDER BY")?;
        let pos = push_unique(targets, v);
        out.push_str(&format!("{pos}:{};", ord_string(&key.options)?));
    }
    Ok(out)
}

/// The value one `ORDER BY` or `DISTINCT ON` key orders or groups by, lowered: the output value it
/// names, or the expression it is lowered in the FROM scope. See [`resolve_order_key`] for which;
/// getting the precedence backwards would silently order by the wrong value in
/// `SELECT b AS a FROM t ORDER BY a`, or in `... ORDER BY t.a`.
///
/// `targets` is read for an output column and is *not* extended here; the caller records the
/// position.
fn order_key_value(
    cat: &Catalog,
    scope: &Scope,
    fns: &Fns,
    e: &Expr,
    targets: &[Value],
    out_cols: &OutCols,
    clause: &str,
) -> Result<Value> {
    match resolve_order_key(e, out_cols, clause)? {
        OrderKey::Output(i) => Ok(targets[i].clone()),
        OrderKey::Input(e) => lower_expr(cat, scope, fns, e),
    }
}

/// A join step in a FROM item's join tree. With both `on` and `precomputed` `None` this is an
/// unrestricted join (a `CROSS JOIN`, or the item's first factor), lowered to a join on `TRUE`. `on`
/// borrows the condition from the FROM AST (lifetime `'a`); `precomputed` carries one we built
/// ourselves, which is how `USING` arrives — its equalities are resolved against the two sides of
/// that one join rather than the finished scope. `upto` is how many bindings the tree has once this
/// step's factor is in: a parenthesized join brings several, so the step's index does not say
/// where its row ends.
struct Step<'a> {
    on: Option<&'a Expr>,
    kind: &'static str,
    precomputed: Option<Value>,
    upto: usize,
}

/// What one FROM factor brings into scope: one binding, or a parenthesized join's several, with
/// its relation and the `USING` names merged inside it.
struct Factor {
    binds: Vec<Binding>,
    rel: Value,
    merged: Vec<String>,
    merged_outer: bool,
    coalesced: Vec<String>,
}

impl Factor {
    fn width(&self) -> usize {
        self.binds.iter().map(|b| b.cols.len()).sum()
    }
}

/// Build the resolution scope and relation tree for a whole FROM clause. `outer` are the
/// enclosing-query bindings; this query's own bindings are offset past them so de-Bruijn indices stay
/// absolute across nesting.
///
/// A comma binds looser than any `JOIN`. Postgres reads `FROM a, b RIGHT JOIN c ON p` as `a`
/// crossed with `b RIGHT JOIN c ON p`, and `p` — or a `USING` there — sees `b` and `c` but not `a`.
/// Folding every item and join into one left-deep chain instead would give
/// `(a CROSS JOIN b) RIGHT JOIN c ON p`, which keeps `c`'s rows when `a` is empty, and would let
/// `p` and `USING` reach `a`. So each comma item is lowered as its own join tree, exactly as a
/// parenthesized join is ([`join_tree_factor`]), and the items' trees are then cross-joined left
/// to right.
fn build_from_clause(
    cat: &Catalog,
    fns: &Fns,
    from: &[TableWithJoins],
    outer: &[Binding],
) -> Result<(Scope, Value)> {
    let base = Scope::outer_width(outer);
    let mut binds: Vec<Binding> = Vec::new();
    let mut offset = base;
    let mut merged: Vec<String> = Vec::new();
    let mut merged_outer = false;
    let mut coalesced: Vec<String> = Vec::new();
    let mut rel: Option<Value> = None;

    for item in from {
        let f = join_tree_factor(cat, fns, item, offset, outer)?;
        offset += f.width();
        merged.extend(f.merged);
        merged_outer |= f.merged_outer;
        coalesced.extend(f.coalesced);
        binds.extend(f.binds);
        rel = Some(match rel {
            None => f.rel,
            Some(left) => {
                let cond = json!({ "operator": "TRUE", "operand": [], "type": "BOOLEAN" });
                json!({ "join": { "condition": cond, "left": left, "right": f.rel, "kind": "INNER" } })
            }
        });
    }
    let inner_count = binds.len();
    binds.extend(outer.iter().cloned()); // outer appended for correlated resolution only
    let scope = Scope { binds, inner_count, base, merged, merged_outer, coalesced };
    Ok((scope, rel.expect("non-empty FROM")))
}

/// One FROM item's join tree (a factor and the joins that follow it), lowered on its own against
/// the enclosing context, as a factor whose row starts at `offset`.
///
/// Its `ON` conditions and `USING` lists may name only its own tables, since Postgres hides the
/// item's FROM siblings from them, and its relation numbers its columns from the enclosing width,
/// like every other join input. Its bindings then move to where its columns sit in the enclosing
/// row. Both a comma item and a parenthesized join are lowered this way.
fn join_tree_factor(
    cat: &Catalog,
    fns: &Fns,
    item: &TableWithJoins,
    offset: usize,
    outer: &[Binding],
) -> Result<Factor> {
    let (inner, rel) = build_join_tree(cat, fns, item, outer)?;
    let shift = offset - Scope::outer_width(outer);
    let binds = inner.inner().iter().map(|b| Binding { offset: b.offset + shift, ..b.clone() }).collect();
    Ok(Factor { binds, rel, merged: inner.merged, merged_outer: inner.merged_outer, coalesced: inner.coalesced })
}

/// The scope and relation of one FROM item's join tree, numbered from the enclosing width: its
/// first factor, then each join in turn, as a left-deep chain (factors may be base tables, derived
/// tables or parenthesized joins).
fn build_join_tree<'a>(
    cat: &Catalog,
    fns: &Fns,
    item: &'a TableWithJoins,
    outer: &[Binding],
) -> Result<(Scope, Value)> {
    let base = Scope::outer_width(outer);
    let mut binds: Vec<Binding> = Vec::new();
    let mut leaves: Vec<Value> = Vec::new();
    let mut steps: Vec<Step<'a>> = Vec::new();
    let mut offset = base;
    let mut merged: Vec<String> = Vec::new();
    let mut merged_outer = false;
    let mut coalesced: Vec<String> = Vec::new();

    let f = from_factor(cat, fns, &item.relation, offset, outer)?;
    offset += f.width();
    merged.extend(f.merged);
    merged_outer |= f.merged_outer;
    coalesced.extend(f.coalesced);
    binds.extend(f.binds);
    leaves.push(f.rel);
    steps.push(Step { on: None, kind: "INNER", precomputed: None, upto: binds.len() });
    for j in &item.joins {
        let f = from_factor(cat, fns, &j.relation, offset, outer)?;
        offset += f.width();
        let (kind, cond) = join_op(&j.join_operator)?;
        // `USING` is resolved here, against the bindings as they stand: its names are looked up
        // on the left of this join and on the factor being added, not through the whole scope.
        let (on, precomputed) = match cond {
            JoinCond::On(e) => (Some(e), None),
            JoinCond::Always => (None, None),
            JoinCond::Using(cols) => {
                // A name a `RIGHT` or `FULL` join has merged is a coalesce of its two sides,
                // which `using_condition` would read as whichever binding has the name first.
                if cols.iter().any(|c| coalesced.contains(c) || f.coalesced.contains(c)) {
                    return Err(unsupported("JOIN ... USING a column a RIGHT or FULL join already merged"));
                }
                let c = using_condition((&binds, &merged), (&f.binds, &f.merged), &cols)?;
                if kind != "INNER" {
                    merged_outer = true;
                }
                if matches!(kind, "RIGHT" | "FULL") {
                    coalesced.extend(cols.iter().cloned());
                }
                merged.extend(cols);
                (None, Some(c))
            }
        };
        merged.extend(f.merged);
        merged_outer |= f.merged_outer;
        coalesced.extend(f.coalesced);
        binds.extend(f.binds);
        leaves.push(f.rel);
        steps.push(Step { on, kind, precomputed, upto: binds.len() });
    }
    let inner_count = binds.len();
    binds.extend(outer.iter().cloned()); // outer appended for correlated resolution only
    let scope = Scope { binds, inner_count, base, merged, merged_outer, coalesced };

    let mut rel: Option<Value> = None;
    for (i, step) in steps.iter().enumerate() {
        let leaf = leaves[i].clone();
        rel = Some(match rel {
            None => leaf,
            Some(left) => {
                let cond = match (&step.precomputed, step.on) {
                    (Some(c), _) => c.clone(),
                    // Only the bindings this join actually has in its row -- see [`Scope::prefix`].
                    (None, Some(on)) => lower_bool(cat, &scope.prefix(step.upto), fns, on)?,
                    (None, None) => json!({ "operator": "TRUE", "operand": [], "type": "BOOLEAN" }),
                };
                json!({ "join": { "condition": cond, "left": left, "right": leaf, "kind": step.kind } })
            }
        });
    }
    Ok((scope, rel.expect("a join tree has a first factor")))
}

/// One FROM factor, lowered.
///
/// A parenthesized join is lowered on its own, as [`join_tree_factor`] describes. The parentheses
/// cannot simply be dropped: `a LEFT JOIN (b JOIN c ON p) ON q` is not
/// `(a LEFT JOIN b ON q) JOIN c ON p`.
fn from_factor(cat: &Catalog, fns: &Fns, tf: &TableFactor, offset: usize, outer: &[Binding]) -> Result<Factor> {
    let TableFactor::NestedJoin { table_with_joins, alias } = tf else {
        let (b, rel) = factor_instance(cat, fns, tf, offset, outer)?;
        let (merged, coalesced) = (Vec::new(), Vec::new());
        return Ok(Factor { binds: vec![b], rel, merged, merged_outer: false, coalesced });
    };
    // `(b JOIN c) AS x` hides `b` and `c` behind one name, over a row that may carry system
    // columns and repeat a name; that is not modelled.
    if alias.is_some() {
        return Err(unsupported("parenthesized join with an alias"));
    }
    join_tree_factor(cat, fns, table_with_joins, offset, outer)
}

/// A single FROM relation factor -> (binding with output columns, leaf relation Value).
fn factor_instance(cat: &Catalog, fns: &Fns, tf: &TableFactor, offset: usize, outer: &[Binding]) -> Result<(Binding, Value)> {
    match tf {
        TableFactor::Table {
            name,
            alias,
            args,
            version,
            with_ordinality,
            partitions,
            json_path,
            sample,
            with_hints: _,
            index_hints: _,
        } => {
            // SOUNDNESS GUARDS: each of these changes which rows the factor yields, and matching on
            // the fields explicitly (rather than `..`) is what keeps a parser upgrade from adding a
            // new one that we then silently drop.
            //   args           a table-valued function, not this catalog table
            //   version        time travel — a different snapshot of the table
            //   with_ordinality  adds a row-number column, so the shape differs
            //   partitions     restricts to some partitions, so rows are missing
            //   json_path      navigates into the value rather than scanning it
            //   sample         TABLESAMPLE, which is not even deterministic
            let modifier = if args.is_some() {
                Some("table-valued function arguments")
            } else if version.is_some() {
                Some("FOR SYSTEM_TIME / version qualifier")
            } else if *with_ordinality {
                Some("WITH ORDINALITY")
            } else if !partitions.is_empty() {
                Some("PARTITION (...)")
            } else if json_path.is_some() {
                Some("PartiQL JSON path")
            } else if sample.is_some() {
                Some("TABLESAMPLE")
            } else {
                None
            };
            if let Some(m) = modifier {
                return Err(unsupported(format!("table factor with {m}")));
            }
            let tn = obj_name(name);
            let idx = cat.find(&tn).ok_or_else(|| schema(format!("unknown table {tn}")))?;
            // An alias's column list renames the table's columns in order, and may stop short:
            // `t AS x(p, q)` makes `x.p` the first column. Resolving by the declared names instead
            // would read `x.a` in `t AS x(b, a)` as the table's own `a`.
            let mut cols = cat.tables[idx].cols.clone();
            if let Some(a) = alias.as_ref().filter(|a| !a.columns.is_empty()) {
                if a.columns.iter().any(|c| c.data_type.is_some()) {
                    return Err(unsupported("column definition list on a table"));
                }
                if a.columns.len() > cat.tables[idx].n_declared {
                    return Err(schema(format!("table {tn} has fewer columns than its alias names")));
                }
                for (col, c) in cols.iter_mut().zip(&a.columns) {
                    col.0 = fold_name(&c.name);
                }
                // Up to case, as `Catalog::check_case_collisions` compares a table's own columns.
                let declared = &cols[..cat.tables[idx].n_declared];
                let same = |m: &str, n: &str| m.to_lowercase() == n.to_lowercase();
                if declared.iter().enumerate().any(|(i, (n, _))| declared[..i].iter().any(|(m, _)| same(m, n))) {
                    return Err(schema(format!("table alias leaves two columns of {tn} with one name")));
                }
            }
            // The name a qualified reference finds this instance by: its alias, which hides the
            // table's own name, or that name as the query spells it. Folded as Postgres folds both.
            let alias = alias.as_ref().map(|a| fold_name(&a.name)).unwrap_or_else(|| last_name(name));
            Ok((
                Binding {
                    alias,
                    cols,
                    offset,
                    table: Some(idx),
                    n_declared: cat.tables[idx].n_declared,
                },
                json!({ "scan": idx }),
            ))
        }
        TableFactor::Derived { subquery, alias, lateral, sample } => {
            // TABLESAMPLE is not deterministic, so it cannot be modelled at all.
            if sample.is_some() {
                return Err(unsupported("derived table with TABLESAMPLE"));
            }
            // A derived table is lowered against the enclosing context only, never its FROM
            // siblings -- which is exactly right for the non-lateral form. A `LATERAL` one *may*
            // see its siblings, and while a reference to one would usually just fail to resolve
            // here, it would not if a sibling shared an alias with an enclosing binding: SQL
            // resolves that to the sibling and we would silently reach the outer one instead.
            if *lateral {
                return Err(unsupported("LATERAL derived table"));
            }
            let (rel, out_cols) = lower_query_ctx(cat, fns, subquery, outer)?;
            let a = alias.as_ref().ok_or_else(|| schema("derived table requires an alias"))?;
            let cols: OutCols = if a.columns.is_empty() {
                // Output names are already folded as Postgres folds them (see [`fold_name`]), which
                // is the form a reference into the binding is looked up in ([`Scope::try_resolve`]).
                out_cols
            } else {
                if a.columns.len() != out_cols.len() {
                    return Err(schema("derived table column-alias count mismatch"));
                }
                a.columns.iter().zip(out_cols).map(|(c, (_, t))| (fold_name(&c.name), t)).collect()
            };
            // Two columns that share a name up to case are refused, as `Catalog::check_case_collisions`
            // refuses them in a table. A reference now tells `"b"` from `"B"`, so this is no longer
            // what keeps them apart; it stays because lifting it is a completeness change of its own.
            let low = |n: &str| n.to_lowercase();
            if let Some(i) = (1..cols.len()).find(|&i| cols[..i].iter().any(|(m, _)| low(m) == low(&cols[i].0))) {
                let name = if is_unnamed(&cols[i].0) { "?column?".to_string() } else { low(&cols[i].0) };
                return Err(unsupported(format!("derived table with two columns named {name} up to case")));
            }
            // No `table`: a derived table has no declared keys, so nothing it outputs can be shown
            // functionally dependent on a GROUP BY key.
            // A derived table's columns are its output, so all of them are visible.
            let n_declared = cols.len();
            Ok((
                Binding { alias: fold_name(&a.name), cols, offset, table: None, n_declared },
                rel,
            ))
        }
        other => Err(unsupported(format!("FROM factor {other:?}"))),
    }
}

/// How a join restricts its two sides.
enum JoinCond<'a> {
    On(&'a Expr),
    /// `CROSS JOIN` / `ON TRUE`: no restriction, just the product.
    Always,
    /// `USING (c, ...)`: an equality per named column, plus a column merge (see [`Scope::merged`]).
    Using(Vec<String>),
}

fn join_op(op: &JoinOperator) -> Result<(&'static str, JoinCond<'_>)> {
    use JoinOperator::*;
    let (kind, c) = match op {
        // `JOIN`/`INNER JOIN`, `LEFT [OUTER]`, `RIGHT [OUTER]`, `FULL OUTER`.
        Join(c) | Inner(c) => ("INNER", c),
        Left(c) | LeftOuter(c) => ("LEFT", c),
        Right(c) | RightOuter(c) => ("RIGHT", c),
        FullOuter(c) => ("FULL", c),
        // A cross join carries `JoinConstraint::None`, which the constraint match below turns
        // into the unrestricted product -- exactly what a cross join is.
        CrossJoin(c) => ("INNER", c),
        other => return Err(unsupported(format!("join operator {other:?}"))),
    };
    match c {
        JoinConstraint::On(e) => Ok((kind, JoinCond::On(e))),
        JoinConstraint::None => Ok((kind, JoinCond::Always)),
        JoinConstraint::Using(names) => {
            let cols = names.iter().map(last_name).collect();
            Ok((kind, JoinCond::Using(cols)))
        }
        other => Err(unsupported(format!("join constraint {other:?}"))),
    }
}

/// The `ON` equalities a `USING (c, ...)` stands for: `left.c = right.c` for each name, where
/// `left` is everything joined so far in this join tree and `right` is the factor being joined in
/// (several bindings, for a parenthesized join). Each side comes with the names `USING` merged
/// inside it.
///
/// Postgres requires the name to be present on each side exactly once, and raises "common column
/// name … appears more than once" otherwise: `a JOIN b ON TRUE JOIN c USING (x)` with `x` in `a`
/// and in `b` has no one left column to compare. A side's copies of a name are its bindings'
/// columns of that name less one for each `USING` inside it that merged two of them into one, so
/// `a JOIN b USING (x) JOIN c USING (x)` has one `x` on the left. When there is one, the first
/// binding that has the name holds its value: the merge of an inner join equals both sides, and
/// a `LEFT` join's is its left side's (a `RIGHT` or `FULL` join's coalesce is refused by the caller).
fn using_condition(
    (left, left_merged): (&[Binding], &[String]),
    (right, right_merged): (&[Binding], &[String]),
    cols: &[String],
) -> Result<Value> {
    let mut terms: Vec<Value> = Vec::new();
    for c in cols {
        let find = |bs: &[Binding], merged: &[String], side: &str| -> Result<(usize, String)> {
            let present: usize = bs.iter().map(|b| b.cols.iter().filter(|(n, _)| n == c).count()).sum();
            let copies = present.saturating_sub(merged.iter().filter(|m| *m == c).count());
            if copies > 1 {
                return Err(schema(format!("common USING column {c} appears more than once on the {side}")));
            }
            bs.iter()
                .find_map(|b| b.cols.iter().position(|(n, _)| n == c).map(|i| (b.offset + i, b.cols[i].1.clone())))
                .ok_or_else(|| schema(format!("USING column {c} not on the {side}")))
        };
        let (li, lt) = find(left, left_merged, "left")?;
        let (ri, rt) = find(right, right_merged, "right")?;
        terms.push(make_cmp("=", json!({ "column": li, "type": lt }), json!({ "column": ri, "type": rt })));
    }
    Ok(match terms.len() {
        0 => json!({ "operator": "TRUE", "operand": [], "type": "BOOLEAN" }),
        1 => terms.pop().unwrap(),
        _ => json!({ "operator": "AND", "operand": terms, "type": "BOOLEAN" }),
    })
}

fn is_pure_wildcard(s: &Select) -> bool {
    s.projection.len() == 1 && matches!(s.projection[0], SelectItem::Wildcard(_))
}

/// Expand a (non-aggregate) projection to `(output name, lowered Value)` pairs, expanding `*` and
/// `t.*` to explicit column references.
fn expand_projection(cat: &Catalog, scope: &Scope, fns: &Fns, s: &Select) -> Result<Vec<(String, Value)>> {
    let mut out: Vec<(String, Value)> = Vec::new();
    for (idx, item) in s.projection.iter().enumerate() {
        match item {
            SelectItem::UnnamedExpr(e) => out.push((expr_name(e, idx), lower_expr(cat, scope, fns, e)?)),
            SelectItem::ExprWithAlias { expr, alias } => {
                out.push((fold_name(alias), lower_expr(cat, scope, fns, expr)?))
            }
            SelectItem::Wildcard(_) => {
                // `USING` merges each named pair into one output column, so a bare `*` here has
                // fewer columns than the same query written with `ON`. We do not model the merge,
                // so expanding `*` would silently give the two forms the same shape. A qualified
                // `t.*` is unaffected (it names one side) and stays supported.
                if !scope.merged.is_empty() {
                    return Err(unsupported("bare * over a JOIN ... USING (merged columns)"));
                }
                // `n_declared`, not `cols.len()`: a Postgres system column is readable by name
                // but `SELECT *` does not return it. See `catalog::add_system_columns`.
                for b in scope.inner() {
                    for (i, (n, t)) in b.cols[..b.n_declared].iter().enumerate() {
                        out.push((n.clone(), json!({ "column": b.offset + i, "type": t })));
                    }
                }
            }
            SelectItem::ExprWithAliases { .. } => {
                return Err(unsupported("multi-alias projection (expr AS (a, b))"))
            }
            SelectItem::QualifiedWildcard(kind, _) => {
                let name = match kind {
                    SelectItemQualifiedWildcardKind::ObjectName(n) => n,
                    SelectItemQualifiedWildcardKind::Expr(_) => {
                        return Err(unsupported("expression.* wildcard"))
                    }
                };
                let q = last_name(name);
                let mut found = false;
                for b in scope.inner() {
                    if b.alias == q {
                        found = true;
                        for (i, (n, t)) in b.cols[..b.n_declared].iter().enumerate() {
                            out.push((n.clone(), json!({ "column": b.offset + i, "type": t })));
                        }
                    }
                }
                if !found {
                    return Err(schema(format!("unknown qualifier {q}.*")));
                }
            }
        }
    }
    Ok(out)
}

/// The output name of the unaliased select-list item `e` at position `idx`: the name Postgres gives
/// it, or, where this frontend cannot tell what that is, an [`unnamed`] placeholder that no key or
/// reference ever matches.
///
/// Postgres names such an item with `FigureColname`: a column reference after the column, a call
/// after the function, a cast after what it casts when that is a column or a call (and after the
/// type otherwise), and an operator or a constant `?column?`. The names are not decoration. A bare
/// `ORDER BY` key is an output column when one has its name ([`resolve_order_key`]), so
/// `SELECT x::int FROM t ORDER BY x` orders by the cast, not by `t.x`.
///
/// By the time this runs the tree has been through the normalizations and the cast rules, which
/// rename some calls (`now()` is `q_op_now`, `ceiling` is `ceil`, a cast may be `qcastN(..)` or gone)
/// and drop or rewrite some type names. Only shapes those passes leave recognisable are named; any
/// other is a placeholder, and a key that could have meant it is refused.
fn expr_name(e: &Expr, idx: usize) -> String {
    implicit_name(e).unwrap_or_else(|| unnamed(idx))
}

/// Postgres's identity for a name: an unquoted identifier folds to lower case, a quoted one is kept
/// as written, so `A`, `a` and `"a"` are one name and `"A"` is another. The rule is
/// [`crate::dml::fold_ident`]'s; every name the scope stores or looks up goes through it.
fn fold_name(id: &sqlparser::ast::Ident) -> String {
    crate::dml::fold_ident(id)
}

/// The name a relation's last name part folds to: the name a table that has no alias is referred to
/// by, as `schema.t` is referred to as `t`. A part that is not an identifier leaves the empty string,
/// which no reference equals.
fn last_name(n: &sqlparser::ast::ObjectName) -> String {
    n.0.last().and_then(|p| p.as_ident()).map(fold_name).unwrap_or_default()
}

/// [`expr_name`]'s `FigureColname`, for the shapes it can still read; `None` for the rest.
fn implicit_name(e: &Expr) -> Option<String> {
    use BinaryOperator::*;
    const OPERATOR: &str = "?column?";
    match e {
        Expr::Nested(x) => implicit_name(x),
        Expr::Value(v) => matches!(
            v.value,
            SqlValue::Number(..) | SqlValue::SingleQuotedString(_) | SqlValue::Boolean(_) | SqlValue::Null
        )
        .then(|| OPERATOR.to_string()),
        // `OVERLAPS` is a call to `overlaps` in Postgres's grammar, so it is not in this list.
        Expr::BinaryOp {
            op: Plus | Minus | Multiply | Divide | Modulo | StringConcat | Gt | Lt | GtEq | LtEq | Eq | NotEq | And | Or,
            ..
        }
        | Expr::UnaryOp { op: UnaryOperator::Not | UnaryOperator::Minus | UnaryOperator::Plus, .. }
        | Expr::IsNull(_)
        | Expr::IsNotNull(_)
        | Expr::IsTrue(_)
        | Expr::IsNotTrue(_)
        | Expr::IsFalse(_)
        | Expr::IsNotFalse(_)
        | Expr::IsUnknown(_)
        | Expr::IsNotUnknown(_)
        | Expr::IsDistinctFrom(..)
        | Expr::IsNotDistinctFrom(..)
        | Expr::Between { .. }
        | Expr::Like { .. }
        | Expr::ILike { .. }
        | Expr::InList { .. }
        | Expr::InSubquery { .. }
        | Expr::AnyOp { .. }
        | Expr::AllOp { .. } => Some(OPERATOR.to_string()),
        _ => call_or_column_name(e),
    }
}

/// The name of a column reference or a call — what Postgres calls a strong name, the kind a cast
/// passes through. A cast over anything else is named after its type, which the cast rules may have
/// rewritten, so it has no name here.
fn call_or_column_name(e: &Expr) -> Option<String> {
    use sqlparser::ast::CastKind;
    match e {
        Expr::Nested(x) => call_or_column_name(x),
        Expr::Identifier(id) => Some(fold_name(id)),
        Expr::CompoundIdentifier(p) => p.last().map(fold_name),
        Expr::Cast { kind: CastKind::Cast | CastKind::DoubleColon, expr, .. } => call_or_column_name(expr),
        Expr::Function(f) => {
            let [.., last] = &f.name.0[..] else { return None };
            let name = fold_name(last.as_ident()?);
            // Cast rule 5b's wrapper (`casts.rs`): the cast it stands for, over its one operand.
            if name.strip_prefix("qcast").is_some_and(|n| n.parse::<u32>().is_ok()) {
                let FunctionArguments::List(l) = &f.args else { return None };
                return match &l.args[..] {
                    [FunctionArg::Unnamed(FunctionArgExpr::Expr(x))] => call_or_column_name(x),
                    _ => None,
                };
            }
            // Symbols the normalizations and the parameter substitution introduce, which do not
            // say what the query called; and `ceil`, which may have been written `ceiling`.
            let invented = name.starts_with("q_")
                || name.strip_prefix("qp").is_some_and(|n| n.parse::<u32>().is_ok())
                || name == "ceil";
            (!invented).then_some(name)
        }
        _ => None,
    }
}

fn group_by_empty(s: &Select) -> bool {
    matches!(&s.group_by, GroupByExpr::Expressions(v, _) if v.is_empty())
}

fn group_by_exprs(s: &Select) -> Result<Vec<&Expr>> {
    match &s.group_by {
        // `GROUP BY (a, b, c)` is a row constructor, and grouping on the row is the same partition
        // as grouping on its members: grouping compares with NULLs equal, and row comparison is
        // field-wise, so two rows agree on the row value exactly when they agree on every member.
        // Flattening it is also what lets the projection reach the members — `SELECT a` matches the
        // key `a`, where against a single row-valued key it would look like a non-grouped column.
        GroupByExpr::Expressions(v, _) => Ok(v
            .iter()
            .flat_map(|e| match e {
                Expr::Tuple(items) => items.iter().collect::<Vec<_>>(),
                other => vec![other],
            })
            .collect()),
        GroupByExpr::All(_) => Err(unsupported("GROUP BY ALL")),
    }
}

fn item_expr(it: &SelectItem) -> Option<&Expr> {
    match it {
        SelectItem::UnnamedExpr(e) => Some(e),
        SelectItem::ExprWithAlias { expr, .. } => Some(expr),
        _ => None,
    }
}

struct Agg {
    op: String,
    args: Vec<Expr>,
    distinct: bool,
    /// The `FILTER (WHERE p)` predicate, evaluated per input row before the fold.
    filter: Option<Expr>,
}

/// One argument of a call, as lowering reads it.
enum CallArg<'a> {
    /// A positional expression.
    Expr(&'a Expr),
    /// A bare `*`, which only `count(*)` gives a meaning to.
    Star,
}

/// Everything call lowering reads of a [`Function`] besides its name: the arguments, whether they are
/// `DISTINCT`, and the `FILTER`. The aggregate path ([`agg_of`]) reads all three; the scalar paths
/// refuse a `DISTINCT`, a `FILTER` and a `*`, each of which says the call is an aggregate.
struct CallParts<'a> {
    args: Vec<CallArg<'a>>,
    distinct: bool,
    filter: Option<&'a Expr>,
}

/// Take a call apart into its [`CallParts`], refusing every other part it can carry.
///
/// SOUNDNESS GUARD. Each of these changes what a call computes, and none has a place in the IR:
/// a named argument (`make_interval(days => a)`, and the SQL/JSON `json_object('k' VALUE a)`, which
/// sqlparser reads as one), a `t.*` argument, `WITHIN GROUP`, an `ORDER BY`, `LIMIT`, `WHERE`,
/// `HAVING`, `SEPARATOR` or `ON OVERFLOW` inside the parentheses, the SQL/JSON `ABSENT ON NULL` and
/// `RETURNING`, `IGNORE NULLS`, a second parameter list, the ODBC `{fn ...}` form, and `OVER`. The
/// call sites used to keep the positional arguments and skip the rest, so `make_interval(days => a)`
/// and `make_interval(hours => a)` both lowered to `MAKE_INTERVAL()`: one term for two different
/// calls, which every prover then proves equal.
///
/// [`Function`] and its argument list are destructured with no `..`, so a field a parser upgrade adds
/// is a compile error here rather than one more part of a call that lowering drops.
fn call_parts(f: &Function) -> Result<CallParts<'_>> {
    let Function { name, uses_odbc_syntax, parameters, args, within_group, filter, null_treatment, over } =
        f;
    if over.is_some() {
        return Err(unsupported("window function (OVER)"));
    }
    if *uses_odbc_syntax {
        return Err(unsupported(format!("ODBC-escaped call {{fn {name}(..)}}")));
    }
    if !matches!(parameters, FunctionArguments::None) {
        return Err(unsupported(format!("second argument list on {name}")));
    }
    if !within_group.is_empty() {
        return Err(unsupported(format!("WITHIN GROUP on {name}")));
    }
    if let Some(n) = null_treatment {
        return Err(unsupported(format!("{n} on {name}")));
    }
    let filter = filter.as_deref();
    let list = match args {
        FunctionArguments::None => return Ok(CallParts { args: Vec::new(), distinct: false, filter }),
        FunctionArguments::Subquery(_) => return Err(unsupported("function with a subquery argument")),
        FunctionArguments::List(l) => l,
    };
    let FunctionArgumentList { duplicate_treatment, args, clauses } = list;
    if let Some(c) = clauses.first() {
        return Err(unsupported(format!("`{c}` in a call to {name}")));
    }
    let args = args
        .iter()
        .map(|a| match a {
            FunctionArg::Unnamed(FunctionArgExpr::Expr(e)) => Ok(CallArg::Expr(e)),
            FunctionArg::Unnamed(FunctionArgExpr::Wildcard) => Ok(CallArg::Star),
            other => Err(unsupported(format!("argument `{other}` in a call to {name}"))),
        })
        .collect::<Result<Vec<_>>>()?;
    let distinct = matches!(duplicate_treatment, Some(DuplicateTreatment::Distinct));
    Ok(CallParts { args, distinct, filter })
}

/// The arguments of a call on a scalar path, where a `DISTINCT`, a `FILTER` or a `*` cannot be lowered.
///
/// SOUNDNESS GUARD. Postgres accepts each of the three only on an aggregate, so a call carrying one is
/// an aggregate this frontend did not recognise, and a per-row reading of it gets the row count wrong.
/// `DISTINCT` used to be dropped here outright.
fn scalar_args<'a>(f: &'a Function, name: &str) -> Result<Vec<&'a Expr>> {
    let parts = call_parts(f)?;
    if parts.distinct || parts.filter.is_some() {
        return Err(unsupported(format!("DISTINCT or FILTER on {name}, which is not a known aggregate")));
    }
    parts
        .args
        .into_iter()
        .map(|a| match a {
            CallArg::Expr(e) => Ok(e),
            CallArg::Star => Err(unsupported(format!("`*` argument to {name}, which is not a known aggregate"))),
        })
        .collect()
}

/// Whether `e` is an aggregate function call: one of the builtins, or a name the input declared with
/// `declare aggregate function`. A windowed call (`OVER`) is not an aggregate for grouping purposes.
fn is_agg_call(fns: &Fns, e: &Expr) -> bool {
    let Expr::Function(f) = e else { return false };
    if f.over.is_some() {
        return false;
    }
    // The names we know on our own are matched on the qualified spelling only — `myschema.sum` is
    // not assumed to be `SUM` (see [`reject_qualified_builtin_agg`], which refuses that case
    // downstream). A *declared* aggregate is matched on either spelling: missing it here is the
    // dangerous direction, since the call would then take the scalar path and inflate the row count.
    let (full, bare) = fn_names(f);
    is_known_agg(&full) || fn_decl(fns, &full, &bare).is_some_and(|d| d.aggregate)
}

/// Whether `e` contains an aggregate call anywhere in *this* query's expression tree.
///
/// `SELECT COALESCE(SUM(a), 0) FROM t` is an aggregate query even though the projection item is a
/// `COALESCE`, so looking only at the top of each item would lower `SUM` as a per-row scalar and
/// silently produce one output row per input row. Recursion deliberately stops at subqueries (an
/// aggregate in there belongs to the subquery, or, when it reads only enclosing columns, is refused
/// there by [`reads_only_enclosing_columns`]) and at windowed calls (not aggregates here).
///
/// This is a *completeness* aid, not the safety net, for every aggregate [`is_agg_call`] recognises:
/// one this walk misses reaches [`lower_expr`]'s function arm, which refuses it, so that gap costs
/// coverage, never soundness. Every built-in Postgres aggregate is either recognised or refused by
/// name there (see [`OPAQUE_AGGS`]), so the guarantee covers them all. What neither can catch is an
/// aggregate a user defined and the input did not declare: to this frontend it is a name like any
/// function's, and it is lowered per row.
fn contains_agg(fns: &Fns, e: &Expr) -> bool {
    if is_agg_call(fns, e) {
        return true;
    }
    let any = |es: &[Expr]| es.iter().any(|x| contains_agg(fns, x));
    match e {
        Expr::Nested(i) | Expr::UnaryOp { expr: i, .. } | Expr::Cast { expr: i, .. } => {
            contains_agg(fns, i)
        }
        Expr::IsNull(i) | Expr::IsNotNull(i) | Expr::IsTrue(i) | Expr::IsNotTrue(i) => {
            contains_agg(fns, i)
        }
        Expr::IsFalse(i) | Expr::IsNotFalse(i) | Expr::IsUnknown(i) | Expr::IsNotUnknown(i) => {
            contains_agg(fns, i)
        }
        Expr::BinaryOp { left, right, .. }
        | Expr::IsDistinctFrom(left, right)
        | Expr::IsNotDistinctFrom(left, right) => contains_agg(fns, left) || contains_agg(fns, right),
        Expr::Between { expr, low, high, .. } => {
            contains_agg(fns, expr) || contains_agg(fns, low) || contains_agg(fns, high)
        }
        Expr::Like { expr, pattern, .. }
        | Expr::ILike { expr, pattern, .. }
        | Expr::SimilarTo { expr, pattern, .. } => {
            contains_agg(fns, expr) || contains_agg(fns, pattern)
        }
        Expr::InList { expr, list, .. } => contains_agg(fns, expr) || any(list),
        Expr::Tuple(es) => any(es),
        Expr::Array(a) => any(&a.elem),
        Expr::CompoundFieldAccess { root, access_chain } => {
            contains_agg(fns, root)
                || access_chain.iter().any(|a| match a {
                    AccessExpr::Subscript(Subscript::Index { index }) => contains_agg(fns, index),
                    _ => false,
                })
        }
        Expr::Case { operand, conditions, else_result, .. } => {
            operand.as_deref().is_some_and(|o| contains_agg(fns, o))
                || conditions
                    .iter()
                    .any(|w| contains_agg(fns, &w.condition) || contains_agg(fns, &w.result))
                || else_result.as_deref().is_some_and(|x| contains_agg(fns, x))
        }
        // A windowed call's arguments are not this query's aggregates, and `OVER` is refused anyway.
        Expr::Function(f) if f.over.is_none() => match &f.args {
            FunctionArguments::List(l) => l.args.iter().any(|a| match a {
                FunctionArg::Unnamed(FunctionArgExpr::Expr(x)) => contains_agg(fns, x),
                _ => false,
            }),
            _ => false,
        },
        _ => false,
    }
}

/// Extract an aggregate (assumes [`is_agg_call`]); refuses unsupported modifiers.
fn agg_of(e: &Expr) -> Result<Agg> {
    let f = match e {
        Expr::Function(f) => f,
        _ => unreachable!("agg_of on non-function"),
    };
    let name = obj_name(&f.name).to_uppercase();
    // SOUNDNESS GUARD: see [`ORDER_SENSITIVE_AGGS`]. The two call-lowering sites already refuse these,
    // but neither is reached from here: an input that says `declare aggregate function array_agg(...)`
    // makes [`is_agg_call`] true and sends the call straight down the Group path. A declaration is a
    // statement about the return type, not permission to assume the bag determines the value.
    reject_order_sensitive_agg(&name, bare_name(&name))?;
    reject_unmodelled_agg(&name, bare_name(&name))?;
    // SOUNDNESS GUARD: see [`call_parts`]. An `ORDER BY` inside the parentheses, a null treatment and
    // `WITHIN GROUP` are among what it refuses; `DISTINCT`, `FILTER` and `*` are read below.
    let CallParts { args: raw_args, distinct, filter } = call_parts(f)?;
    // `FILTER (WHERE p)` is lowered by pushing the predicate into the argument as
    // `CASE WHEN p THEN arg END` (see [`AggCtx::add_agg`]), which is only faithful for aggregates
    // that skip NULL inputs. That is exactly the builtins: for anything else -- a declared
    // aggregate such as `QA_OP_ARRAYAGG` -- the rewrite would feed it a NULL per non-matching row
    // instead of dropping the row, and `array_agg` keeps NULLs. Refuse rather than guess.
    if filter.is_some() && !BUILTIN_AGGS.contains(&name.as_str()) {
        return Err(unsupported(format!("FILTER on the non-builtin aggregate {name}")));
    }
    let mut args = Vec::new();
    for a in raw_args {
        match a {
            CallArg::Star => {}
            CallArg::Expr(e) => args.push(e.clone()),
        }
    }
    // `COUNT(*) FILTER (WHERE p)` counts matching rows, so it needs *something* to count; `1` is
    // the standard stand-in and makes the rewrite below `COUNT(CASE WHEN p THEN 1 END)`.
    if filter.is_some() && args.is_empty() {
        if name != "COUNT" {
            return Err(unsupported(format!("FILTER on argument-less {name}")));
        }
        args.push(Expr::Value(
            SqlValue::Number("1".to_string(), false).with_empty_span(),
        ));
    }
    Ok(Agg { op: name, args, distinct, filter: filter.cloned() })
}

/// State for lowering expressions over a group's *output* scope (keys first, then aggregate results).
/// Aggregates encountered are accumulated into `funcs` (with their args appended to `preproj`).
///
/// Both scopes this juggles — the pre-aggregation projection and the group output — are binders the
/// frontend introduces, so positions in them become levels only after adding [`Scope::base`]
/// (reachable through `scope`). See that field for why.
struct AggCtx<'a> {
    cat: &'a Catalog,
    scope: &'a Scope, // the FROM (pre-aggregation) scope
    fns: &'a Fns,
    key_vals: Vec<Value>,
    m: usize,
    preproj: Vec<Value>, // initialised to key_vals; aggregate args appended
    funcs: Vec<Value>,   // AggCall JSONs; output position = m + index
}

impl AggCtx<'_> {
    fn key_match(&self, e: &Expr) -> Result<Option<usize>> {
        // An expression containing an aggregate can never *be* a GROUP BY key, and lowering it in
        // the pre-aggregation scope would trip the aggregate-in-scalar-position guard.
        if contains_agg(self.fns, e) {
            return Ok(None);
        }
        let v = lower_expr(self.cat, self.scope, self.fns, e)?;
        Ok(self.key_vals.iter().position(|k| *k == v))
    }

    fn add_agg(&mut self, a: Agg) -> Result<Value> {
        // `agg(x) FILTER (WHERE p)` folds over the rows where `p` holds. There is nothing in the
        // prover's `AggCall` to say so, but for an aggregate that skips NULL inputs the standard
        // rewrite says it anyway: `agg(CASE WHEN p THEN x END)` hands the fold a NULL for every
        // non-matching row, and the `ignoreNulls` flag set below then drops exactly those rows.
        // `p` is evaluated in the pre-aggregation scope, like the aggregate's own arguments.
        // [`agg_of`] has already restricted this to the builtins, whose null-skipping is what makes
        // the rewrite an identity rather than an approximation.
        let filter = match &a.filter {
            Some(p) => Some(lower_bool(self.cat, self.scope, self.fns, p)?),
            None => None,
        };
        let mut read = Vec::new();
        if let Some(p) = &filter {
            ir_levels(p, &mut read);
        }
        let mut lowered = Vec::new();
        for arg in &a.args {
            let v = lower_expr(self.cat, self.scope, self.fns, arg)?;
            ir_levels(&v, &mut read);
            lowered.push(v);
        }
        if reads_only_enclosing_columns(self.scope, &read) {
            return Err(unsupported(format!(
                "aggregate {} over columns of an enclosing query only (it belongs to that query)",
                a.op
            )));
        }
        reject_float_summing(&a.op, &lowered)?;
        let mut operand: Vec<Value> = Vec::new();
        for mut v in lowered {
            if let Some(p) = &filter {
                // A NULL of the argument's own type, so `make_case` finds the branches already in
                // agreement and leaves the NULL uncast -- a cast one would stop testing as null.
                let null_v = json!({ "operator": "NULL", "operand": [], "type": ty_of(&v) });
                v = make_case(vec![p.clone(), v, null_v]);
            }
            let pos = self.preproj.len();
            let ty = ty_of(&v);
            self.preproj.push(v);
            operand.push(json!({ "column": self.scope.base + pos, "type": ty }));
        }
        // The declaration is looked up the same way [`is_agg_call`] found it — qualified, then bare.
        // Keying on `a.op` alone would recognise `public.myagg` as an aggregate and then miss its
        // declared return type, falling through to the argument's type below.
        let declared = fn_decl(self.fns, &a.op, bare_name(&a.op))
            .filter(|d| d.aggregate)
            .map(|d| d.ret.clone());
        let rty = if a.op == "COUNT" {
            "INTEGER".to_string()
        } else if let Some(d) = declared {
            d
        } else if let Some(t) = opaque_agg_ret(&a.op) {
            t.to_string()
        } else if let Some(f) = operand.first() {
            ty_of(f)
        } else {
            "INTEGER".to_string()
        };
        // `ignoreNulls` tells the prover whether to restrict the aggregate's input to rows where
        // every argument is non-NULL. That is exactly the semantics of the builtins when they have
        // arguments: `COUNT(x)`/`SUM(x)`/`AVG`/`MIN`/`MAX` all skip NULL inputs. Emitting `false`
        // here (as Calcite's `ignoreNulls()` does, since it means the unrelated `IGNORE NULLS`
        // window modifier) makes the prover count NULL rows, which collapses `COUNT(x)` into
        // `COUNT(*)` -- a false positive.
        //
        // Two cases must stay `false`:
        //   * no arguments (`COUNT(*)`): the prover applies the filter to the *whole source row*,
        //     which would count only rows with no NULL in any column.
        //   * uninterpreted aggregates, whether declared or from [`OPAQUE_AGGS`]: their null
        //     handling is not something we model (`array_agg` keeps NULLs, `string_agg` drops them),
        //     and `false` is the incomplete-not-unsound direction -- it distinguishes bags that
        //     differ only in NULLs rather than conflating them.
        let ignore_nulls = !operand.is_empty() && BUILTIN_AGGS.contains(&a.op.as_str());
        self.funcs.push(json!({
            "operator": a.op, "operand": operand, "type": rty,
            "distinct": a.distinct, "ignoreNulls": ignore_nulls,
        }));
        Ok(json!({ "column": self.scope.base + self.m + self.funcs.len() - 1, "type": rty }))
    }

    /// Lower an expression that lives in the post-aggregation scope (group keys + aggregate results).
    fn lower_post(&mut self, e: &Expr) -> Result<Value> {
        if is_agg_call(self.fns, e) {
            let a = agg_of(e)?;
            return self.add_agg(a);
        }
        if let Some(j) = self.key_match(e)? {
            return Ok(json!({ "column": self.scope.base + j, "type": ty_of(&self.key_vals[j]) }));
        }
        match e {
            Expr::Nested(i) => self.lower_post(i),
            Expr::Value(v) => lower_value(&v.value),
            // A sub-chain can itself be a GROUP BY key, so the descent stops at one.
            Expr::BinaryOp { left, op, right } if matches!(op, BinaryOperator::And | BinaryOperator::Or) => {
                let parts = chain_operands(left, op, right, |x| Ok(self.key_match(x)?.is_some()))?;
                let parts =
                    parts.into_iter().map(|x| Ok(coerce_bool(self.lower_post(x)?))).collect::<Result<_>>()?;
                Ok(connective(op, parts))
            }
            Expr::BinaryOp { left, op, right } => {
                use BinaryOperator::*;
                let l = self.lower_post(left)?;
                let r = self.lower_post(right)?;
                let (s, t) = binop(op, &l, &r)?;
                if matches!(op, Eq | NotEq | Lt | Gt | LtEq | GtEq) {
                    // A group key or an aggregate is not a column of the scope, so its collation is
                    // not traced here: it is the default one only when no column the pair reads
                    // declares another (see `collation`).
                    let cat = self.cat;
                    crate::collation::compare(&s, l, r, |_, _| {
                        if crate::collation::varies(cat) {
                            Err(unsupported(
                                "an order comparison of strings after grouping, where a column the pair reads \
                                 declares a collation",
                            ))
                        } else {
                            Ok(Some(crate::collation::Collation::Default))
                        }
                    })
                } else {
                    Ok(make_arith(&s, l, r, &t))
                }
            }
            Expr::UnaryOp { op, expr } => {
                let inner = self.lower_post(expr)?;
                unary(op, inner)
            }
            Expr::IsNull(i) => Ok(json!({ "operator": "IS NULL", "operand": [self.lower_post(i)?], "type": "BOOLEAN" })),
            Expr::IsNotNull(i) => Ok(json!({ "operator": "IS NOT NULL", "operand": [self.lower_post(i)?], "type": "BOOLEAN" })),
            Expr::Cast { expr, data_type, .. } => {
                Ok(lower_cast(self.lower_post(expr)?, data_type))
            }
            Expr::Case { operand, conditions, else_result, .. } => {
                if operand.is_some() {
                    return Err(unsupported("simple CASE in post-aggregate position"));
                }
                let mut ops: Vec<Value> = Vec::new();
                for w in conditions {
                    ops.push(self.lower_post(&w.condition)?);
                    ops.push(self.lower_post(&w.result)?);
                }
                let else_v = match else_result {
                    Some(e) => self.lower_post(e)?,
                    None => json!({ "operator": "NULL", "operand": [], "type": "INTEGER" }),
                };
                ops.push(else_v);
                Ok(make_case(ops))
            }
            Expr::Function(f) => {
                // a scalar function over keys/aggregates (is_agg_call already returned false)
                let (name, bare) = fn_names(f);
                reject_qualified_builtin_agg(&name, &bare)?;
                reject_nondeterministic(&name, &bare)?;
                reject_order_sensitive_agg(&name, &bare)?;
                reject_unmodelled_agg(&name, &bare)?;
                // SOUNDNESS GUARD, the same as `lower_expr`'s: a set-returning function over
                // aggregates is no more a scalar than one over columns, and lowering it as one
                // understates the row count.
                if SET_RETURNING.contains(&bare.as_str()) {
                    return Err(unsupported(format!("set-returning function {name} in scalar position")));
                }
                let operand =
                    scalar_args(f, &name)?.into_iter().map(|e| self.lower_post(e)).collect::<Result<Vec<_>>>()?;
                let ret = crate::equality::call_type(&name, &operand, fn_ret(self.fns, &name, &bare));
                Ok(json!({ "operator": name, "operand": operand, "type": ret }))
            }
            Expr::Identifier(_) | Expr::CompoundIdentifier(_) => {
                Err(unsupported("column not functionally dependent on GROUP BY"))
            }
            Expr::Array(arr) => {
                array_shape(arr)?;
                let elems = arr.elem.iter().map(|x| self.lower_post(x)).collect::<Result<Vec<_>>>()?;
                Ok(array_call(elems))
            }
            Expr::CompoundFieldAccess { root, access_chain } => {
                let mut operand = vec![self.lower_post(root)?];
                for i in subscript_indices(access_chain)? {
                    operand.push(self.lower_post(i)?);
                }
                Ok(subscript_call(operand))
            }
            other => Err(unsupported(format!("post-aggregate expression {other:?}"))),
        }
    }
}

/// Refuse a pattern spelled `ALL(..)`, `ANY(..)` or `SOME(..)`.
///
/// sqlparser reads `s LIKE ALL($1)` as a `LIKE` whose pattern is a call to a function named `ALL`
/// (only `LIKE ANY` gets its own flag, refused above). Lowered that way it is one match against one
/// opaque pattern, which a prover reads as strict -- but the quantified form is not:
/// `NULL LIKE ALL('{}')` is TRUE, as an `ALL` over no elements is.
fn refuse_quantified_pattern(op: &str, pattern: &Expr) -> Result<()> {
    if let Expr::Function(f) = pattern {
        if let [part] = f.name.0.as_slice() {
            if let Some(id) = part.as_ident() {
                let q = id.value.to_uppercase();
                if id.quote_style.is_none() && matches!(q.as_str(), "ALL" | "ANY" | "SOME") {
                    return Err(unsupported(format!("{op} {q}(..)")));
                }
            }
        }
    }
    Ok(())
}

/// The absolute column level a lowered expression *is*, if it is a bare column reference.
fn plain_col(v: &Value) -> Option<usize> {
    if v.get("operand").is_some() {
        return None;
    }
    v.get("column").and_then(|c| c.as_u64()).map(|c| c as usize)
}

/// Whether the grouped levels pin down `level` — i.e. whether they contain every column of some
/// declared key of the same binding.
///
/// Postgres accepts a non-grouped column when it is functionally dependent on the GROUP BY, and this
/// is that rule, restricted to the one dependence a `CREATE TABLE` proves: group on a key of a table
/// and each group holds rows from a single row of that table, so every other column of it is constant
/// within the group.
///
/// Both restrictions are load-bearing:
///
///   * **the key's columns must be NOT NULL.** `UNIQUE` alone permits many rows with a NULL key, and
///     `GROUP BY` puts all of them in one group — so on `T(u UNIQUE, b)` holding `{(NULL,1),
///     (NULL,2)}`, `GROUP BY u` is one group and `b` is not constant in it. `PRIMARY KEY` implies
///     NOT NULL, so the common case passes; a nullable `UNIQUE` is refused.
///   * **the key must be grouped on the same binding.** [`Scope::binding_of`] resolves the level to
///     a relation *instance*, so under `t AS a JOIN t AS b`, grouping on `a.id` does not license
///     reading `b.name`.
///
/// An outer join does not break it. If the binding is on the nullable side, an unmatched row has
/// NULL for the whole key *and* for the dependent column; since a real row cannot have a NULL key,
/// the all-NULL group is exactly the unmatched rows and the column is constant (NULL) across it.
fn key_determines(cat: &Catalog, scope: &Scope, grouped: &[usize], level: usize) -> bool {
    let Some((b, _)) = scope.binding_of(level) else { return false };
    let Some(t) = b.table else { return false };
    let tbl = &cat.tables[t];
    tbl.keys.iter().any(|k| {
        !k.is_empty() && k.iter().all(|&ci| !tbl.nullable[ci] && grouped.contains(&(b.offset + ci)))
    })
}

/// Every column level a lowered expression references.
///
/// `{"column": n}` is the IR's only way to name a value from the scope, so collecting that one key
/// over the whole tree cannot miss a reference — including references buried in a subquery, which is
/// the case [`constant_per_group`] needs and which a walk of the *SQL* cannot promise without an arm
/// for every expression variant. The other integer the IR carries, `{"scan": i}`, is a catalog index
/// rather than a level and is correctly ignored.
fn ir_levels(v: &Value, out: &mut Vec<usize>) {
    match v {
        Value::Object(o) => {
            for (k, x) in o {
                match (k.as_str(), x.as_u64()) {
                    ("column", Some(n)) => out.push(n as usize),
                    _ => ir_levels(x, out),
                }
            }
        }
        Value::Array(a) => a.iter().for_each(|x| ir_levels(x, out)),
        _ => {}
    }
}

/// Whether an aggregate whose arguments and `FILTER` read the column levels `read` belongs to an
/// enclosing query rather than to the one being lowered over `scope`.
///
/// SOUNDNESS GUARD. Postgres gives an aggregate to the innermost query level its arguments read a
/// column of: if the arguments and the `FILTER` contain only outer-level variables, "the aggregate
/// then belongs to the nearest such outer level, and is evaluated over the rows of that query"
/// (manual, 4.2.7). In `SELECT (SELECT count(t.a) FROM u) FROM t`, `count(t.a)` folds over `t` and
/// makes the outer query an aggregate one, returning one row. Lowered where it is written, it folds
/// over `u` instead, once per row of `t`. The IR has no way to say an aggregate is an enclosing
/// query's, so such a call is refused.
///
/// Levels are absolute ([`crate::scope`]), so the test is arithmetic: below [`Scope::base`] is an
/// enclosing query's column, within this query's own bindings is its own, and above both is a column
/// bound by a subquery inside the argument, which Postgres does not count either. An aggregate that
/// reads no column at all (`count(*)`, `count(1)`) is this query's.
fn reads_only_enclosing_columns(scope: &Scope, read: &[usize]) -> bool {
    let own = |l: usize| scope.inner().iter().any(|b| l >= b.offset && l < b.offset + b.cols.len());
    read.iter().any(|&l| l < scope.base) && !read.iter().any(|&l| own(l))
}

/// Whether a lowered expression takes a single value within each group.
///
/// [`key_determines`] lifted from a column to an expression: an expression is a function of the
/// columns it reads, so if every one of them is constant within a group then so is it.
fn constant_per_group(cat: &Catalog, scope: &Scope, grouped: &[usize], v: &Value) -> bool {
    let mut levels = Vec::new();
    ir_levels(v, &mut levels);
    levels.iter().all(|&l| {
        // A level no binding in scope covers was minted by a binder *inside* the expression — a
        // subquery's own FROM — so it is bound there and ranges over that subquery's rows, not this
        // query's. Nesting numbers those levels above every binding in scope (a nested query's own
        // columns start at the enclosing context's total width), so they cannot be mistaken for one.
        scope.binding_of(l).is_none()
            || grouped.contains(&l)
            || key_determines(cat, scope, grouped, l)
    })
}

/// Collect the expressions a post-aggregation expression reads, already lowered — the candidates for
/// the key list. Each is a candidate only; [`extend_with_determined`] decides.
///
/// Mirrors [`AggCtx::lower_post`]'s traversal, and the two ways it differs from a plain walk are the
/// point. It stops at an aggregate call, because those arguments are read per input row rather than
/// per group and turning one into a group key would split the very groups it folds over — `GROUP BY
/// t.id ... sum(p.amount)` must not group on `p.amount`. And it descends only through the shapes
/// `lower_post` descends through, so a variant missing here is one `lower_post` refuses anyway: the
/// cost of falling behind it is a refusal, never a key that should not be there.
fn post_columns(cat: &Catalog, scope: &Scope, fns: &Fns, e: &Expr, out: &mut Vec<Value>) {
    if is_agg_call(fns, e) {
        return;
    }
    match e {
        Expr::Identifier(id) => {
            if let Ok(v) = col_ref(scope, None, &fold_name(id)) {
                out.push(v);
            }
        }
        Expr::CompoundIdentifier(parts) => {
            if let [q, col] = &parts[..] {
                if let Ok(v) = col_ref(scope, Some(&fold_name(q)), &fold_name(col)) {
                    out.push(v);
                }
            }
        }
        Expr::Nested(i)
        | Expr::UnaryOp { expr: i, .. }
        | Expr::IsNull(i)
        | Expr::IsNotNull(i)
        | Expr::Cast { expr: i, .. } => post_columns(cat, scope, fns, i, out),
        Expr::BinaryOp { left, right, .. } => {
            post_columns(cat, scope, fns, left, out);
            post_columns(cat, scope, fns, right, out);
        }
        Expr::Array(a) => {
            for x in &a.elem {
                post_columns(cat, scope, fns, x, out);
            }
        }
        Expr::CompoundFieldAccess { root, access_chain } => {
            post_columns(cat, scope, fns, root, out);
            for a in access_chain {
                if let AccessExpr::Subscript(Subscript::Index { index }) = a {
                    post_columns(cat, scope, fns, index, out);
                }
            }
        }
        Expr::Case { conditions, else_result, .. } => {
            for w in conditions {
                post_columns(cat, scope, fns, &w.condition, out);
                post_columns(cat, scope, fns, &w.result, out);
            }
            if let Some(x) = else_result {
                post_columns(cat, scope, fns, x, out);
            }
        }
        Expr::Function(f) => {
            if let Ok(parts) = call_parts(f) {
                for a in parts.args {
                    if let CallArg::Expr(x) = a {
                        post_columns(cat, scope, fns, x, out);
                    }
                }
            }
        }
        // The subquery-bearing shapes, which `lower_post` has no post-aggregate rule for at all. The
        // dependence lifts from a column to a whole expression unchanged — see [`constant_per_group`]
        // — so offering the expression itself as a key is what lets `lower_post` match it. Whether it
        // *is* determined is decided there; an expression that is not simply stays refused, as
        // `GROUP BY $5, $6` with a projection reading `tags` must.
        //
        // Offered whole rather than descended into: none of the columns inside are reachable
        // individually in the post-group scope, so collecting them would only propose keys that
        // split groups. The list is an allowlist so that adding a shape is a deliberate act; leaving
        // one out costs a refusal, never a wrong key.
        // The guard is because an aggregate of *this* query inside the candidate would be lowered in
        // the wrong scope. One inside the subquery belongs to the subquery, and `contains_agg` stops
        // at that boundary, so `EXISTS (SELECT ... sum(x) ...)` is still offered.
        Expr::Exists { .. }
        | Expr::Subquery(_)
        | Expr::InSubquery { .. }
        | Expr::AnyOp { .. }
        | Expr::AllOp { .. }
            if !contains_agg(fns, e) =>
        {
            if let Ok(v) = lower_expr(cat, scope, fns, e) {
                out.push(v);
            }
        }
        _ => {}
    }
}

/// Append the columns the GROUP BY functionally determines to the key list, so they lower as keys.
///
/// The rewrite is to group on `keys ++ determined` instead of `keys`. Under the dependence that is
/// the same partition — every determined column is already constant within a group, so adding it
/// splits nothing — and it is the only lowering available: the prover's post-group scope holds the
/// keys and the aggregate results, with no way to name a column that is neither.
///
/// Dependence is tested against the *declared* keys only, computed before any column is appended, so
/// the result does not depend on the order candidates are visited.
fn extend_with_determined(cat: &Catalog, scope: &Scope, fns: &Fns, s: &Select, key_vals: &mut Vec<Value>) {
    let grouped: Vec<usize> = key_vals.iter().filter_map(plain_col).collect();
    if grouped.is_empty() {
        return;
    }
    let mut cands: Vec<Value> = Vec::new();
    for it in &s.projection {
        if let Some(e) = item_expr(it) {
            post_columns(cat, scope, fns, e, &mut cands);
        }
    }
    if let Some(h) = &s.having {
        post_columns(cat, scope, fns, h, &mut cands);
    }
    for v in cands {
        if key_vals.contains(&v) {
            continue;
        }
        if constant_per_group(cat, scope, &grouped, &v) {
            key_vals.push(v);
        }
    }
}

/// Lower one `GROUP BY` item, falling back to a select-list alias when the FROM scope has no such
/// column.
///
/// ```text
/// SELECT event_id AS eid, count(*) FROM events GROUP BY eid
/// ```
///
/// Postgres accepts that: a **bare, unqualified** `GROUP BY` name may name an output column. The
/// keys here are lowered against the FROM scope, where an alias the projection introduces does not
/// exist — and it cannot simply be looked up in `out_cols`, because those do not exist yet either
/// (the projection is lowered *after* the grouping, over the post-aggregation scope). So the alias
/// is resolved syntactically, out of `s.projection`, and the expression it names is lowered in its
/// place. That is the same relation `GROUP BY <that expression>` would produce, which is why
/// [`extend_with_determined`] and the identity elision above keep working unchanged: the key holds
/// the expression's value either way, so the projection item carrying the alias still matches it.
///
/// **The FROM scope is tried first, and that ordering is the rule rather than an optimization.**
/// Postgres resolves a `GROUP BY` name as an input column when one exists and only then as an
/// output column, so an input column of the same name has to win.
///
/// Everything else declines rather than guesses:
///
/// * a **qualified** name (`t.x`) never denotes an output column, so the fallback is not taken;
/// * nor for a name the FROM scope has but refused to read (an outer `USING`'s merged column), or
///   that may be a column whose name is not known: either is an input column in Postgres;
/// * **two select items sharing the alias** is a resolution question with no right answer, refused
///   exactly as [`resolve_order_key`] refuses the same ambiguity for `ORDER BY`;
/// * an alias over an **aggregate** is rejected by Postgres itself, so lowering it would be
///   lowering a query that does not run;
/// * when the alias path also fails, the **original** error is what is reported — the fallback
///   should not move a row's blocker onto a construct that was never the cause.
fn lower_group_key(
    cat: &Catalog,
    scope: &Scope,
    fns: &Fns,
    s: &Select,
    e: &Expr,
) -> Result<Value> {
    // `GROUP BY 1` is a 1-based position in the select list, not the integer 1, and nothing here
    // resolves it — `group_by_exprs` has no positional arm, so it would lower as the literal and
    // group every row together. There are no such rows in any corpus measured, so this is a guard
    // against a silent wrong answer rather than a feature declined. A non-integer constant is
    // refused by Postgres outright, which lands in the same place.
    if let Expr::Value(v) = e {
        if let SqlValue::Number(n, _) = &v.value {
            return Err(unsupported(format!("GROUP BY position {n}")));
        }
    }
    let err = match lower_expr(cat, scope, fns, e) {
        Ok(v) => return Ok(v),
        Err(err) => err,
    };
    let Expr::Identifier(id) = e else { return Err(err) };
    // Only a name that no input column could be falls back to the select list. One that a binding
    // has, or that may be a column whose name is not known ([`Scope::try_resolve`]'s `Err`), is an
    // input column in Postgres, whatever this frontend made of it, so it keeps its refusal.
    if !matches!(scope.try_resolve(None, &fold_name(id)), Ok(None)) {
        return Err(err);
    }
    match alias_target(fns, s, id)? {
        Some(target) => lower_expr(cat, scope, fns, target),
        None => Err(err),
    }
}

/// The expression a select-list alias names, for [`lower_group_key`]'s fallback.
///
/// `Ok(None)` means no item carries the alias — the caller keeps its original error. `Err` is for
/// the two cases where an item does carry it but using it would be wrong.
fn alias_target<'a>(fns: &Fns, s: &'a Select, id: &sqlparser::ast::Ident) -> Result<Option<&'a Expr>> {
    let name = fold_name(id);
    let mut found = s.projection.iter().filter_map(|it| match it {
        SelectItem::ExprWithAlias { expr, alias } if fold_name(alias) == name => Some(expr),
        _ => None,
    });
    match (found.next(), found.next()) {
        (Some(e), None) if contains_agg(fns, e) => {
            Err(schema(format!("aggregate in GROUP BY key {name}")))
        }
        (Some(e), None) => Ok(Some(e)),
        (Some(_), Some(_)) => Err(schema(format!("ambiguous GROUP BY key {name}"))),
        _ => Ok(None),
    }
}

fn lower_aggregate(cat: &Catalog, scope: &Scope, fns: &Fns, rel: Value, s: &Select) -> Result<(Value, OutCols)> {
    let keys = group_by_exprs(s)?;
    let mut key_vals: Vec<Value> = keys
        .iter()
        .map(|e| lower_group_key(cat, scope, fns, s, e))
        .collect::<Result<Vec<_>>>()?;
    // Must happen before `m` is read: the aggregate-result levels are numbered from the end of the
    // key list, so a key appended later would shift every one of them.
    extend_with_determined(cat, scope, fns, s, &mut key_vals);
    let m = key_vals.len();
    let mut ag = AggCtx { cat, scope, fns, key_vals: key_vals.clone(), m, preproj: key_vals.clone(), funcs: Vec::new() };

    // Lower SELECT items and HAVING over the post-aggregation scope (both may introduce aggregates).
    let mut out_cols: OutCols = Vec::new();
    let mut targets: Vec<Value> = Vec::new();
    for (idx, it) in s.projection.iter().enumerate() {
        let e = item_expr(it).ok_or_else(|| unsupported(format!("aggregate projection item {it:?}")))?;
        let v = ag.lower_post(e)?;
        let name = match it {
            SelectItem::ExprWithAlias { alias, .. } => fold_name(alias),
            _ => expr_name(e, idx),
        };
        out_cols.push((name, ty_of(&v)));
        targets.push(v);
    }
    let having = match &s.having {
        Some(h) => Some(coerce_bool(ag.lower_post(h)?)),
        None => None,
    };

    // Build Group over the pre-projection (keys ++ aggregate args).
    let source = if ag.preproj.is_empty() {
        rel
    } else {
        json!({ "project": { "target": ag.preproj, "source": rel } })
    };
    let gkeys: Vec<Value> = key_vals
        .iter()
        .enumerate()
        .map(|(i, v)| json!({ "column": scope.base + i, "type": ty_of(v) }))
        .collect();
    let mut result = json!({ "group": { "keys": gkeys, "function": ag.funcs, "source": source } });

    if let Some(cond) = having {
        result = json!({ "filter": { "condition": cond, "source": result } });
    }

    // Elide the top projection when SELECT is exactly the group output in order (matches Calcite).
    let n_out = m + result_group_func_count(&result);
    let is_identity = targets.len() == n_out
        && targets.iter().enumerate().all(|(i, t)| {
            t.get("column").and_then(|c| c.as_u64()) == Some((scope.base + i) as u64)
                && t.get("operand").is_none()
        });
    if !is_identity {
        result = json!({ "project": { "target": targets, "source": result } });
    }
    Ok((result, out_cols))
}

/// Number of aggregate functions in a group / filter(group) result (for the identity-projection check).
fn result_group_func_count(v: &Value) -> usize {
    let g = v.get("group").or_else(|| v.get("filter").and_then(|f| f.get("source")).and_then(|s| s.get("group")));
    g.and_then(|g| g.get("function")).and_then(|f| f.as_array()).map(|a| a.len()).unwrap_or(0)
}

/// Apply a unary operator to an already-lowered operand.
fn unary(op: &UnaryOperator, inner: Value) -> Result<Value> {
    match op {
        UnaryOperator::Plus => Ok(inner),
        UnaryOperator::Minus => {
            let ty = ty_of(&inner);
            Ok(json!({ "operator": "-", "operand": [inner], "type": ty }))
        }
        UnaryOperator::Not => Ok(json!({ "operator": "NOT", "operand": [inner], "type": "BOOLEAN" })),
        other => Err(unsupported(format!("unary op {other:?}"))),
    }
}

/// `left op right` for a comparison operator. An operand may name `C` or `POSIX` with `COLLATE`,
/// which decides this comparison's collation and nothing else; an order comparison of two strings
/// is native only under such a collation, and the uninterpreted predicate for its collation under
/// any other ([`crate::collation`]).
fn lower_comparison(
    cat: &Catalog,
    scope: &Scope,
    fns: &Fns,
    op: &BinaryOperator,
    left: &Expr,
    right: &Expr,
) -> Result<Value> {
    let (left, lc) = crate::collation::strip(left)?;
    let (right, rc) = crate::collation::strip(right)?;
    let l = lower_expr(cat, scope, fns, left)?;
    let r = lower_expr(cat, scope, fns, right)?;
    let (opstr, _) = binop(op, &l, &r)?;
    crate::collation::compare(&opstr, l, r, |a, b| {
        crate::collation::comparison(cat, scope, lc || rc, &[(left, a), (right, b)])
    })
}

/// Lower a scalar expression to an `Expr` Value.
fn lower_expr(cat: &Catalog, scope: &Scope, fns: &Fns, e: &Expr) -> Result<Value> {
    match e {
        Expr::Nested(inner) => lower_expr(cat, scope, fns, inner),
        Expr::Identifier(id) => col_ref(scope, None, &fold_name(id)),
        Expr::CompoundIdentifier(parts) => {
            let col = fold_name(parts.last().unwrap());
            let qual = (parts.len() >= 2).then(|| fold_name(&parts[parts.len() - 2]));
            col_ref(scope, qual.as_deref(), &col)
        }
        Expr::Value(v) => lower_value(&v.value),
        Expr::Function(f) => {
            if f.over.is_some() {
                return Err(unsupported("window function (OVER)"));
            }
            // SOUNDNESS GUARD: an aggregate reaching the scalar path would be lowered as a per-row
            // function, quietly turning one output row into one row per input row. `contains_agg`
            // routes aggregates to the Group path before we get here, so anything still arriving is
            // a case it does not model (e.g. an aggregate in WHERE, or nested in a form it does not
            // walk) and must be refused rather than mis-lowered.
            if is_agg_call(fns, e) {
                let name = obj_name(&f.name).to_uppercase();
                return Err(unsupported(format!("aggregate {name} in scalar position")));
            }
            let (name, bare) = fn_names(f);
            reject_qualified_builtin_agg(&name, &bare)?;
            reject_nondeterministic(&name, &bare)?;
            reject_order_sensitive_agg(&name, &bare)?;
            reject_unmodelled_agg(&name, &bare)?;
            // SOUNDNESS GUARD: see [`SET_RETURNING`] — these are not scalars and lowering them as
            // one would understate the row count. Matched on the *bare* name: `public.unnest(x)` is
            // still `unnest`, and widening a refusal can only ever cost completeness.
            if SET_RETURNING.contains(&bare.as_str()) {
                return Err(unsupported(format!("set-returning function {name} in scalar position")));
            }
            let operand = scalar_args(f, &name)?
                .into_iter()
                .map(|e| lower_expr(cat, scope, fns, e))
                .collect::<Result<Vec<_>>>()?;
            let ret = crate::equality::call_type(&name, &operand, fn_ret(fns, &name, &bare));
            Ok(json!({ "operator": name, "operand": operand, "type": ret }))
        }
        // Row-constructor comparison: `(a, b) = (x, y)`. The standard defines row `=` as the
        // conjunction of the pairwise comparisons, three-valued logic included — true iff every
        // pair is true, false iff some pair is false, unknown otherwise — which is exactly what
        // `AND` over the pairwise `=` yields. Row `<>` is defined as the negation of row `=`, so it
        // is the same expansion under a `NOT`. The ordering comparisons are lexicographic rather
        // than pairwise, so they get no expansion here and are refused.
        Expr::BinaryOp { left, op, right }
            if matches!(**left, Expr::Tuple(_)) || matches!(**right, Expr::Tuple(_)) =>
        {
            let (le, re) = match (left.as_ref(), right.as_ref()) {
                (Expr::Tuple(l), Expr::Tuple(r)) => (l, r),
                _ => return Err(unsupported("row comparison against a non-row operand")),
            };
            if le.len() != re.len() {
                return Err(schema(format!(
                    "row comparison arity: {} column(s) on the left, {} on the right",
                    le.len(),
                    re.len()
                )));
            }
            let negated = match op {
                BinaryOperator::Eq => false,
                BinaryOperator::NotEq => true,
                other => return Err(unsupported(format!("row comparison with {other:?}"))),
            };
            let ls: Vec<Value> =
                le.iter().map(|x| lower_expr(cat, scope, fns, x)).collect::<Result<_>>()?;
            let m = row_match(&ls, re, cat, scope, fns)?;
            Ok(if negated { not_bool(m) } else { m })
        }
        Expr::BinaryOp { left, op, right } if matches!(op, BinaryOperator::And | BinaryOperator::Or) => {
            let parts = chain_operands(left, op, right, |_| Ok(false))?;
            let parts =
                parts.into_iter().map(|x| Ok(coerce_bool(lower_expr(cat, scope, fns, x)?))).collect::<Result<_>>()?;
            Ok(connective(op, parts))
        }
        Expr::BinaryOp { left, op, right } => {
            use BinaryOperator::*;
            if matches!(op, Eq | NotEq | Lt | Gt | LtEq | GtEq) {
                return lower_comparison(cat, scope, fns, op, left, right);
            }
            let l = lower_expr(cat, scope, fns, left)?;
            let r = lower_expr(cat, scope, fns, right)?;
            let (opstr, ty) = binop(op, &l, &r)?;
            Ok(make_arith(&opstr, l, r, &ty))
        }
        Expr::UnaryOp { op, expr } => {
            let inner = lower_expr(cat, scope, fns, expr)?;
            unary(op, inner)
        }
        Expr::IsNull(inner) => {
            Ok(json!({ "operator": "IS NULL", "operand": [lower_expr(cat, scope, fns, inner)?], "type": "BOOLEAN" }))
        }
        Expr::IsNotNull(inner) => {
            Ok(json!({ "operator": "IS NOT NULL", "operand": [lower_expr(cat, scope, fns, inner)?], "type": "BOOLEAN" }))
        }
        // The three-valued `IS <truth value>` tests. The prover interprets `IS TRUE` as "this
        // expression evaluates to true" and `IS NOT TRUE` as its complement (so NULL satisfies
        // `IS NOT TRUE`), which is exactly SQL. The other four are not interpreted natively but
        // are definable from the two that are, without losing the NULL case:
        //   `x IS FALSE`       == `(NOT x) IS TRUE`      (NOT NULL is NULL, so NULL fails both)
        //   `x IS NOT FALSE`   == `(NOT x) IS NOT TRUE`
        //   `x IS UNKNOWN`     == `x IS NULL`            (UNKNOWN *is* the NULL boolean)
        //   `x IS NOT UNKNOWN` == `x IS NOT NULL`
        Expr::IsTrue(i) | Expr::IsNotTrue(i) | Expr::IsFalse(i) | Expr::IsNotFalse(i) => {
            let v = coerce_bool(lower_expr(cat, scope, fns, i)?);
            let (negate_operand, op) = match e {
                Expr::IsTrue(_) => (false, "IS TRUE"),
                Expr::IsNotTrue(_) => (false, "IS NOT TRUE"),
                Expr::IsFalse(_) => (true, "IS TRUE"),
                _ => (true, "IS NOT TRUE"),
            };
            let v = if negate_operand { not_bool(v) } else { v };
            Ok(json!({ "operator": op, "operand": [v], "type": "BOOLEAN" }))
        }
        Expr::IsUnknown(i) => {
            Ok(json!({ "operator": "IS NULL", "operand": [lower_expr(cat, scope, fns, i)?], "type": "BOOLEAN" }))
        }
        Expr::IsNotUnknown(i) => {
            Ok(json!({ "operator": "IS NOT NULL", "operand": [lower_expr(cat, scope, fns, i)?], "type": "BOOLEAN" }))
        }
        Expr::IsDistinctFrom(a, b) => {
            Ok(make_cmp("IS DISTINCT FROM", lower_expr(cat, scope, fns, a)?, lower_expr(cat, scope, fns, b)?))
        }
        Expr::IsNotDistinctFrom(a, b) => {
            Ok(make_cmp("IS NOT DISTINCT FROM", lower_expr(cat, scope, fns, a)?, lower_expr(cat, scope, fns, b)?))
        }
        // x IN (a, b, ...) -> OR(x = a, ...);  (l1,l2) IN ((a1,a2),...) -> OR(AND(l1=a1, l2=a2), ...)
        Expr::InList { expr, list, negated } => {
            let join = if *negated { "AND" } else { "OR" };
            let cmp = if *negated { "<>" } else { "=" };
            let mut terms: Vec<Value> = Vec::new();
            if let Expr::Tuple(lhs_elems) = expr.as_ref() {
                let ls: Vec<Value> =
                    lhs_elems.iter().map(|e| lower_expr(cat, scope, fns, e)).collect::<Result<Vec<_>>>()?;
                for item in list {
                    let mut bare = item;
                    while let Expr::Nested(inner) = bare {
                        bare = inner;
                    }
                    let relems = match bare {
                        Expr::Tuple(r) => r,
                        // A parameter standing for a whole row is a composite value, and Postgres
                        // compares a row with a composite value under record semantics, where two
                        // NULL fields are equal -- not field by field as against a row constructor.
                        // So the item is one opaque predicate over the row and the value, never
                        // expanded into per-field comparisons.
                        p if crate::infer::param_index(p).is_some() => {
                            let mut operand = ls.clone();
                            operand.push(lower_expr(cat, scope, fns, p)?);
                            let m = json!({
                                "operator": format!("q_row_eq_{}", ls.len()), "operand": operand, "type": "BOOLEAN",
                            });
                            terms.push(if *negated { not_bool(m) } else { m });
                            continue;
                        }
                        _ => return Err(unsupported("row IN list item that is neither a row nor a parameter")),
                    };
                    if relems.len() != ls.len() {
                        return Err(unsupported("row IN arity mismatch"));
                    }
                    let m = row_match(&ls, relems, cat, scope, fns)?;
                    terms.push(if *negated { not_bool(m) } else { m });
                }
            } else {
                let lhs = lower_expr(cat, scope, fns, expr)?;
                for item in list {
                    terms.push(make_cmp(cmp, lhs.clone(), lower_expr(cat, scope, fns, item)?));
                }
            }
            Ok(if terms.len() == 1 {
                terms.into_iter().next().unwrap()
            } else {
                json!({ "operator": join, "operand": terms, "type": "BOOLEAN" })
            })
        }
        Expr::Case { operand, conditions, else_result, .. } => {
            let mut ops: Vec<Value> = Vec::new();
            for w in conditions {
                let cond = match operand {
                    Some(scrut) => make_cmp(
                        "=",
                        lower_expr(cat, scope, fns, scrut)?,
                        lower_expr(cat, scope, fns, &w.condition)?,
                    ),
                    None => lower_expr(cat, scope, fns, &w.condition)?,
                };
                ops.push(cond);
                ops.push(lower_expr(cat, scope, fns, &w.result)?);
            }
            let else_v = match else_result {
                Some(e) => lower_expr(cat, scope, fns, e)?,
                None => json!({ "operator": "NULL", "operand": [], "type": "INTEGER" }),
            };
            ops.push(else_v);
            Ok(make_case(ops))
        }
        // LIKE, ILIKE and SIMILAR TO are uninterpreted boolean operators, kept distinct by name: the
        // prover has no rule for any of them, so it treats each as an uninterpreted function of its
        // arguments and never unifies one with another. `ESCAPE` and the Snowflake `LIKE ANY` form
        // change the matching semantics and are not modelled, so they are refused rather than dropped.
        Expr::Like { negated, expr, pattern, escape_char, any }
        | Expr::ILike { negated, expr, pattern, escape_char, any } => {
            let op = if matches!(e, Expr::ILike { .. }) { "ILIKE" } else { "LIKE" };
            if *any {
                return Err(unsupported(format!("{op} ANY")));
            }
            refuse_quantified_pattern(op, pattern)?;
            lower_match(cat, scope, fns, op, *negated, expr, pattern, escape_char.is_some())
        }
        Expr::SimilarTo { negated, expr, pattern, escape_char } => {
            refuse_quantified_pattern("SIMILAR TO", pattern)?;
            lower_match(cat, scope, fns, "SIMILAR TO", *negated, expr, pattern, escape_char.is_some())
        }
        // Postgres reads `e BETWEEN lo AND hi` as `e >= lo AND e <= hi`, and gives each of the two
        // comparisons its own collation: a `COLLATE` on `e` decides both, one on `lo` the first.
        Expr::Between { expr, negated, low, high } => {
            let (expr, ec) = crate::collation::strip(expr)?;
            let (low, lc) = crate::collation::strip(low)?;
            let (high, hc) = crate::collation::strip(high)?;
            let e = lower_expr(cat, scope, fns, expr)?;
            let lo = lower_expr(cat, scope, fns, low)?;
            let hi = lower_expr(cat, scope, fns, high)?;
            let ge = crate::collation::compare(">=", e.clone(), lo, |a, b| {
                crate::collation::comparison(cat, scope, ec || lc, &[(expr, a), (low, b)])
            })?;
            let le = crate::collation::compare("<=", e, hi, |a, b| {
                crate::collation::comparison(cat, scope, ec || hc, &[(expr, a), (high, b)])
            })?;
            Ok(if *negated {
                json!({ "operator": "OR", "operand": [not_bool(ge), not_bool(le)], "type": "BOOLEAN" })
            } else {
                json!({ "operator": "AND", "operand": [ge, le], "type": "BOOLEAN" })
            })
        }
        Expr::Cast { expr, data_type, .. } => {
            Ok(lower_cast(lower_expr(cat, scope, fns, expr)?, data_type))
        }
        Expr::InSubquery { expr, subquery, negated } => {
            lower_in_subquery(cat, scope, fns, expr, subquery, *negated)
        }
        // `x = ANY(...)` / `x <> ALL(...)` -- see [`lower_quantified`], which also explains why the
        // other comparison operators are refused.
        Expr::AnyOp { left, compare_op, right, .. } => {
            lower_quantified(cat, scope, fns, left, compare_op, right, false)
        }
        Expr::AllOp { left, compare_op, right } => {
            lower_quantified(cat, scope, fns, left, compare_op, right, true)
        }
        Expr::Exists { subquery, negated } => {
            let sub = lower_query_ctx(cat, fns, subquery, &scope.binds)?.0;
            let v = json!({ "operator": "EXISTS", "operand": [], "query": sub, "type": "BOOLEAN" });
            Ok(if *negated { not_bool(v) } else { v })
        }
        // A scalar subquery: the value of the single column of its single row (NULL if it returns no
        // row). Emitted as `$SCALAR_QUERY` -- the spelling Calcite's parser uses for the same thing --
        // which the prover reads as a higher-order operator: an uninterpreted value keyed on the
        // *normalised* subquery relation and the outer row.
        //
        // Sound in both directions. Two of these collapse to one value exactly when their relations
        // normalise equal, and equal relations denote the same bag, hence the same scalar. Otherwise
        // they stay unrelated, so nothing is equated by accident. And because the value is a fresh
        // universally-quantified variable, a proof that holds for every value of it holds for the one
        // SQL actually produces -- which is why the missing 0-row/NULL and >1-row/error cases cost
        // completeness here rather than soundness.
        //
        // It is *more* conservative than `EXISTS`/`IN`, which the prover special-cases into
        // interpreted logic over the relation. This one lands on the generic memoised branch
        // (`normal.rs:790`), whose key includes the whole in-scope substitution vector -- so two sides
        // that differ only in the *order* of their bindings get distinct variables even where the
        // relations normalise equal. Commuting a join under a correlated scalar subquery is the
        // visible case: provable with `EXISTS`, not provable with this. Only ever loses proofs.
        Expr::Subquery(q) => {
            let (rel, cols) = lower_query_ctx(cat, fns, q, &scope.binds)?;
            if cols.len() != 1 {
                return Err(schema(format!(
                    "scalar subquery selects {} columns, expected 1",
                    cols.len()
                )));
            }
            Ok(json!({
                "operator": "$SCALAR_QUERY", "operand": [], "query": rel, "type": cols[0].1
            }))
        }
        Expr::Array(arr) => {
            array_shape(arr)?;
            let elems = arr.elem.iter().map(|x| lower_expr(cat, scope, fns, x)).collect::<Result<Vec<_>>>()?;
            Ok(array_call(elems))
        }
        Expr::CompoundFieldAccess { root, access_chain } => {
            let mut operand = vec![lower_expr(cat, scope, fns, root)?];
            for i in subscript_indices(access_chain)? {
                operand.push(lower_expr(cat, scope, fns, i)?);
            }
            Ok(subscript_call(operand))
        }
        other => Err(unsupported(format!("expr: {other:?}"))),
    }
}

/// Lower an expression in a boolean (predicate) context: recurse through AND/OR/NOT so every leaf is
/// lowered as a predicate, coercing non-boolean leaf operators to BOOLEAN.
fn lower_bool(cat: &Catalog, scope: &Scope, fns: &Fns, e: &Expr) -> Result<Value> {
    use BinaryOperator::{And, Or};
    match e {
        Expr::Nested(i) => lower_bool(cat, scope, fns, i),
        Expr::BinaryOp { left, op, right } if matches!(op, And | Or) => {
            let parts = chain_operands(left, op, right, |_| Ok(false))?;
            let parts = parts.into_iter().map(|x| lower_bool(cat, scope, fns, x)).collect::<Result<_>>()?;
            Ok(connective(op, parts))
        }
        Expr::UnaryOp { op: UnaryOperator::Not, expr } => Ok(not_bool(lower_bool(cat, scope, fns, expr)?)),
        _ => Ok(coerce_bool(lower_expr(cat, scope, fns, e)?)),
    }
}

/// The operands of the `AND` or `OR` chain that `left op right` heads, left to right, looking through
/// parentheses. `atomic` stops the descent at a node that is to be lowered whole.
///
/// Iterative on purpose: generated predicates run to hundreds of terms, a chain parses left-deep,
/// and recursing down it would cost a stack frame per term.
fn chain_operands<'e>(
    left: &'e Expr,
    op: &BinaryOperator,
    right: &'e Expr,
    mut atomic: impl FnMut(&Expr) -> Result<bool>,
) -> Result<Vec<&'e Expr>> {
    let mut out = Vec::new();
    let mut todo = vec![right, left];
    while let Some(e) = todo.pop() {
        if atomic(e)? {
            out.push(e);
            continue;
        }
        match e {
            Expr::Nested(i) => todo.push(i),
            Expr::BinaryOp { left, op: o, right } if o == op => todo.extend([&**right, &**left]),
            _ => out.push(e),
        }
    }
    Ok(out)
}

/// One n-ary `AND` or `OR` node over lowered operands, splicing in any operand that is itself a node
/// of the same operator.
///
/// Sound by associativity, which holds in three-valued logic. The IR stays as shallow as the
/// predicate is wide (the prover's case reader has a nesting limit), and a chain lowers like any of
/// its regroupings. Nothing is spliced across the two operators, and a `NOT` stays where it is.
fn connective(op: &BinaryOperator, operands: Vec<Value>) -> Value {
    let name = if matches!(op, BinaryOperator::And) { "AND" } else { "OR" };
    let mut flat = Vec::with_capacity(operands.len());
    for v in operands {
        match v {
            Value::Object(mut m)
                if m.get("operator").and_then(Value::as_str) == Some(name)
                    && m.get("operand").is_some_and(Value::is_array) =>
            {
                if let Some(Value::Array(a)) = m.remove("operand") {
                    flat.extend(a);
                }
            }
            v => flat.push(v),
        }
    }
    json!({ "operator": name, "operand": flat, "type": "BOOLEAN" })
}

/// `expr IN (subquery)`, and its negation. Non-correlated and correlated alike: the subquery sees
/// the current row scope as outer.
///
/// The left side may be a row constructor: `(a, b) IN (SELECT x, y FROM t)`. The prover handles that
/// natively — it zips the operands with the subquery's output columns and conjoins the equalities —
/// but it *asserts* the two have the same width, so a mismatch panics it. Check the arity here and
/// refuse instead.
///
/// Shared with [`lower_quantified`], because `x = ANY (SELECT ..)` is not merely equivalent to this,
/// it is the same predicate spelled differently — so it had better lower to the same IR.
fn lower_in_subquery(
    cat: &Catalog,
    scope: &Scope,
    fns: &Fns,
    expr: &Expr,
    subquery: &Query,
    negated: bool,
) -> Result<Value> {
    let lhs: Vec<Value> = match expr {
        Expr::Tuple(elems) => {
            elems.iter().map(|x| lower_expr(cat, scope, fns, x)).collect::<Result<_>>()?
        }
        one => vec![lower_expr(cat, scope, fns, one)?],
    };
    let (sub, sub_cols) = lower_query_ctx(cat, fns, subquery, &scope.binds)?;
    if sub_cols.len() != lhs.len() {
        return Err(schema(format!(
            "IN subquery arity: {} column(s) on the left, {} selected",
            lhs.len(),
            sub_cols.len()
        )));
    }
    // The prover compares each left operand with the subquery's column as it stands, with no
    // coercion of its own, so a DATE against a TIMESTAMP column would compare two units.
    let lhs: Vec<Value> = lhs
        .into_iter()
        .zip(&sub_cols)
        .map(|(x, (_, t))| coerce_in_operand(x, t).map_err(|m| unsupported(format!("IN subquery: {m}"))))
        .collect::<Result<_>>()?;
    let v = json!({ "operator": "IN", "operand": lhs, "query": sub, "type": "BOOLEAN" });
    Ok(if negated { not_bool(v) } else { v })
}

/// `x = ANY(rhs)` and `x <> ALL(rhs)`, in each of the operand shapes Postgres allows on the right.
///
/// ## Why only those two comparison operators
///
/// Postgres allows any comparison under either quantifier, and the prover looks like it agrees: it
/// reads an operator of the form `"<cmp> <quant>"` off a relation-valued node and evaluates it with
/// `quant_cmp`, which implements the three-valued truth table exactly (`normal.rs:517`). But for the
/// *ordered* comparisons that function reaches `fn cmp`, which opens with
/// `assert!(matches!(ty, Integer | Real | String))` and takes no type guard on the way in — unlike
/// the plain binary-comparison path, which checks the operand type before dispatching. So
/// `d > ANY (SELECT ..)` over a DATE or VARBINARY column does not cost a proof, it *panics the
/// prover*. `= ANY` and `<> ALL` take the equality branch instead, which goes through `self.equal`
/// and is total.
///
/// The remaining two pairings, `= ALL` and `<> ANY`, take that same total equality branch and so
/// are not a panic hazard — but neither is `IN`, so they would need the relation-valued form, which
/// nothing in this frontend emits yet. They are refused for want of a case to justify testing it.
///
/// Refusing them costs nothing measurable: in practice a quantified comparison is essentially always
/// `= ANY` or `<> ALL`, and both of those are handled.
///
/// ## An aggregate underneath one is still refused
///
/// [`contains_agg`] does not walk into these nodes, so `count(*) = ANY(..)` on its own does not mark
/// the query as aggregated. That is a coverage gap, not the [`OPAQUE_AGGS`] hazard again — both ways
/// out of it are refusals, never a demotion. On the scalar path the aggregate reaches [`lower_expr`]'s
/// function arm and is refused as "in scalar position" (pinned by a test); where the query is
/// aggregated for some other reason, the expression reaches `lower_post`, which has no arm for these
/// and refuses too (one corpus case does exactly that).
fn lower_quantified(
    cat: &Catalog,
    scope: &Scope,
    fns: &Fns,
    left: &Expr,
    op: &BinaryOperator,
    right: &Expr,
    all: bool,
) -> Result<Value> {
    let quant = if all { "ALL" } else { "ANY" };
    if !matches!((all, op), (false, BinaryOperator::Eq) | (true, BinaryOperator::NotEq)) {
        return Err(unsupported(format!(
            "{op} {quant} (only `= ANY` and `<> ALL` are lowered)"
        )));
    }
    // Parentheses around the operand are syntax: `ANY((SELECT ..))` is `ANY(SELECT ..)`.
    let mut rhs = right;
    while let Expr::Nested(inner) = rhs {
        rhs = inner;
    }
    match rhs {
        // `x = ANY (SELECT ..)` *is* `x IN (SELECT ..)`, and `x <> ALL (SELECT ..)` is its negation
        // — the same three-valued truth table, not just agreement on non-NULL input. The prover
        // reaches identical logic either way (`IN` is literally `quant_cmp("SOME", "=", ..)`), and
        // going through the same function here makes the two spellings produce identical IR, which
        // is what lets a rewrite between them be proved.
        Expr::Subquery(q) => lower_in_subquery(cat, scope, fns, left, q, all),

        // `x = ANY (ARRAY[a, b, c])` -> `(x = a) OR (x = b) OR (x = c)`, and ALL -> AND over `<>`.
        // Exact, NULLs included: three-valued OR is TRUE when some disjunct is, NULL when none is
        // and one is NULL, and FALSE otherwise — ANY's truth table verbatim, and dually for ALL.
        // The empty array lands on the connective's identity, and SQL agrees with that too: `= ANY`
        // of nothing is FALSE, `<> ALL` of nothing is TRUE.
        //
        // This is the one shape the Python preprocessor already rewrites (`desugar_any_all`), so it
        // never survives to reach us today. It is here so that stage does not have to be ported.
        Expr::Array(arr) => {
            let l = lower_expr(cat, scope, fns, left)?;
            let cmp = if all { "<>" } else { "=" };
            let elems: Vec<Value> =
                arr.elem.iter().map(|e| lower_expr(cat, scope, fns, e)).collect::<Result<_>>()?;
            // The expansion compares `x` with each *element*, which is only what `ANY` does when
            // every element is a scalar: over `ARRAY[t.tags]`, with `tags` an array, `ANY` ranges
            // over the leaves of the two-dimensional result, not over `tags` itself. Arrays lower
            // to the opaque VARBINARY, as do other values that are not arrays, so a column or an
            // expression of opaque type is refused rather than guessed at. A parameter or a literal
            // is a scalar whatever its inferred type: under the binding contract each `$N` is one
            // value, typed by what it is compared with.
            // By this point `casts::rewrite` has spelled each `$N` as a call `qpN(0)`.
            let leaf = |e: &Expr| {
                let mut e = e;
                while let Expr::Nested(inner) = e {
                    e = inner;
                }
                match e {
                    Expr::Value(_) => true,
                    Expr::Function(f) => {
                        let n = obj_name(&f.name).to_lowercase();
                        n.strip_prefix("qp").is_some_and(|d| !d.is_empty() && d.bytes().all(|b| b.is_ascii_digit()))
                    }
                    _ => false,
                }
            };
            if arr.elem.iter().zip(&elems).any(|(e, v)| !leaf(e) && is_opaque(&ty_of(v))) {
                return Err(unsupported(format!(
                    "{op} {quant} over an ARRAY[..] with an element of opaque type"
                )));
            }
            let terms: Vec<Value> = elems.into_iter().map(|v| make_cmp(cmp, l.clone(), v)).collect();
            Ok(match terms.len() {
                0 => {
                    let lit = if all { "TRUE" } else { "FALSE" };
                    json!({ "operator": lit, "operand": [], "type": "BOOLEAN" })
                }
                1 => terms.into_iter().next().unwrap(),
                _ => {
                    let join = if all { "AND" } else { "OR" };
                    json!({ "operator": join, "operand": terms, "type": "BOOLEAN" })
                }
            })
        }

        // Everything else — an array-valued parameter, an array-typed column, `ARRAY(..)` — has no
        // element list to expand and no relation to quantify over. It becomes an uninterpreted
        // boolean symbol applied to the two operands, the same treatment `LIKE` gets.
        //
        // Sound because it is a *function*: `x = ANY(A)` is determined by `x` and `A` alone, so the
        // real semantics is one of the interpretations the prover quantifies over, and whatever it
        // proves for all of them holds for that one. Two things have to be part of the symbol's
        // identity for that to survive, and both are in the name: the comparison operator and the
        // quantifier, so `= ANY` can never unify with `<> ALL`.
        //
        // The prover leaves the symbol genuinely free. `shared.rs:app` declares it over the *option*
        // sorts and asserts no axiom about it — in particular not the strict NULL propagation
        // `null.rs` builds into the operators it does model. That distinction is load-bearing here,
        // because `= ANY` is not strict: `NULL = ANY(ARRAY[])` is FALSE, not NULL. A strictness
        // axiom would have been an unsound assumption about this symbol; there isn't one.
        _ => {
            let l = lower_expr(cat, scope, fns, left)?;
            let r = lower_expr(cat, scope, fns, rhs)?;
            Ok(json!({
                "operator": format!("{op} {quant}"), "operand": [l, r], "type": "BOOLEAN"
            }))
        }
    }
}

/// Positive row match: `AND(l_i = r_i)` for a row-constructor comparison.
fn row_match(ls: &[Value], relems: &[Expr], cat: &Catalog, scope: &Scope, fns: &Fns) -> Result<Value> {
    let mut eqs: Vec<Value> = Vec::new();
    for (l, re) in ls.iter().zip(relems) {
        eqs.push(make_cmp("=", l.clone(), lower_expr(cat, scope, fns, re)?));
    }
    Ok(if eqs.len() == 1 {
        eqs.into_iter().next().unwrap()
    } else {
        json!({ "operator": "AND", "operand": eqs, "type": "BOOLEAN" })
    })
}

/// Refuse the array constructors [`array_call`] does not take: the empty one, whose element type
/// only a cast around it can say, and the bare `[..]`, which is not Postgres.
fn array_shape(arr: &sqlparser::ast::Array) -> Result<()> {
    if !arr.named {
        return Err(unsupported("array constructor without ARRAY"));
    }
    if arr.elem.is_empty() {
        return Err(unsupported("empty ARRAY[]"));
    }
    Ok(())
}

/// `ARRAY[e1, .., en]` as an opaque constructor over its elements, each converted to their common
/// type, and named after that type.
///
/// Opaque, and so not strict: `ARRAY[NULL]` is a one-element array, not NULL. The element type is in
/// the name because the IR carries every array as one opaque type, and `ARRAY[1]` and `ARRAY['1']`
/// are different values. Only reached outside `= ANY(..)`, where the constructor is expanded
/// instead (`lower_quantified`).
///
/// An array of values whose `=` is not identity compares its elements with that `=`, so it is named
/// after them ([`COARSE_OPAQUE`]): VARBINARY to the provers, and to [`crate::equality`] an array of
/// `numeric`. An array of values whose `=` is identity is [`IDENTITY_OPAQUE`].
fn array_call(elems: Vec<Value>) -> Value {
    let ty = elems.iter().map(ty_of).reduce(|a, b| common_type(&a, &b)).unwrap_or_else(|| "VARBINARY".into());
    let operand: Vec<Value> = elems.into_iter().map(|v| cast_to(v, &ty)).collect();
    let array = coarse_class(&ty).map_or_else(|| IDENTITY_OPAQUE.to_string(), |c| format!("{COARSE_OPAQUE}{c}[]"));
    json!({ "operator": format!("q_array_{}", name_part(&ty).to_lowercase()), "operand": operand, "type": array })
}

/// The indices of a subscript chain `a[i]..[j]`, refusing a slice and a field selection.
fn subscript_indices(chain: &[AccessExpr]) -> Result<Vec<&Expr>> {
    chain
        .iter()
        .map(|a| match a {
            AccessExpr::Subscript(Subscript::Index { index }) => Ok(index),
            AccessExpr::Subscript(Subscript::Slice { .. }) => Err(unsupported("array slice")),
            AccessExpr::Dot(_) => Err(unsupported("field selected from a composite value")),
        })
        .collect()
}

/// `a[i]..[j]` as one opaque call over the base and every index, named after their types.
///
/// One call, not one per index, because the chain is not a composition: over a two-dimensional `m`,
/// `m[1][2]` is an element, while `(m[1])[2]` is NULL, since a subscript with too few indices is.
/// Typed VARBINARY, because the element type is not something the IR carries (an array is opaque
/// whatever it holds).
///
/// An element of an array whose elements' `=` is not identity, or a part of a `jsonb`, is such a
/// value too, and named after it ([`COARSE_OPAQUE`]). Any other is a value of a type the frontend does
/// not know, plain VARBINARY, though the array's `=` is identity: what a subscript yields is not
/// always an element of the same kind, and `point`'s `p[0]` is a float.
fn subscript_call(operand: Vec<Value>) -> Value {
    let types: Vec<String> = operand.iter().map(|v| name_part(&ty_of(v)).to_lowercase()).collect();
    let element = operand
        .first()
        .and_then(|base| coarse_class(&ty_of(base)).map(|c| opaque_of_class(c.trim_end_matches("[]"))))
        .unwrap_or_else(|| "VARBINARY".to_string());
    json!({ "operator": format!("q_subscript_{}", types.join("_")), "operand": operand, "type": element })
}

/// Lower one of the pattern-matching predicates to its named uninterpreted operator.
#[allow(clippy::too_many_arguments)]
fn lower_match(
    cat: &Catalog,
    scope: &Scope,
    fns: &Fns,
    op: &str,
    negated: bool,
    expr: &Expr,
    pattern: &Expr,
    has_escape: bool,
) -> Result<Value> {
    if has_escape {
        return Err(unsupported(format!("{op} ... ESCAPE")));
    }
    let l = lower_expr(cat, scope, fns, expr)?;
    let p = lower_expr(cat, scope, fns, pattern)?;
    let v = json!({ "operator": op, "operand": [l, p], "type": "BOOLEAN" });
    Ok(if negated { not_bool(v) } else { v })
}

/// A column reference, by its folded qualifier and name (see [`fold_name`]).
fn col_ref(scope: &Scope, qual: Option<&str>, name: &str) -> Result<Value> {
    // Under an outer `JOIN ... USING`, an unqualified merged name is the preserved side's value
    // (`COALESCE` of both, for FULL) -- not whichever binding resolution happens to reach first.
    if scope.merged_conflict(qual, name) {
        return Err(unsupported(format!("unqualified {name} merged by an outer JOIN ... USING")));
    }
    match scope.try_resolve(qual, name)? {
        Some((idx, ty)) => Ok(json!({ "column": idx, "type": ty })),
        None => Err(schema(format!(
            "unresolved column {}{} (correlated subquery or unknown column)",
            qual.map(|q| format!("{q}.")).unwrap_or_default(),
            name
        ))),
    }
}

/// A literal. Numbers and strings are emitted as `types` encodes constants: see
/// [`number_literal`] and [`string_literal`].
fn lower_value(v: &SqlValue) -> Result<Value> {
    use SqlValue::*;
    Ok(match v {
        Number(n, _) => number_literal(n).map_err(unsupported)?,
        SingleQuotedString(s) | DoubleQuotedString(s) | NationalStringLiteral(s) => string_literal(s),
        Boolean(b) => {
            json!({ "operator": if *b { "TRUE" } else { "FALSE" }, "operand": [], "type": "BOOLEAN" })
        }
        Null => json!({ "operator": "NULL", "operand": [], "type": "INTEGER" }),
        other => return Err(unsupported(format!("literal {other:?}"))),
    })
}

/// Operator string and result type for a binary operator (operand coercion is applied by the caller
/// for comparisons via [`make_cmp`]).
fn binop(op: &BinaryOperator, l: &Value, r: &Value) -> Result<(String, String)> {
    use BinaryOperator::*;
    let s = match op {
        Eq => "=",
        NotEq => "<>",
        Lt => "<",
        Gt => ">",
        LtEq => "<=",
        GtEq => ">=",
        Plus => "+",
        Minus => "-",
        Multiply => "*",
        Divide => "/",
        Modulo => "%",
        And => "AND",
        Or => "OR",
        StringConcat => "||",
        other => return Err(unsupported(format!("binary op {other:?}"))),
    };
    let ty = match op {
        Eq | NotEq | Lt | Gt | LtEq | GtEq | And | Or => "BOOLEAN".to_string(),
        StringConcat => "VARCHAR".to_string(),
        _ if ty_of(l) == "REAL" || ty_of(r) == "REAL" => "REAL".to_string(),
        _ => "INTEGER".to_string(),
    };
    Ok((s.to_string(), ty))
}
