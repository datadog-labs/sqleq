// Unless explicitly stated otherwise all files in this repository are licensed under the
// Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/).
// Copyright 2026-Present Datadog, Inc.

//! Postgres functions DuckDB does not have, supplied as macros.
//!
//! The pairs under test are Postgres SQL; the oracle is DuckDB. Where a Postgres function has no
//! DuckDB counterpart, *both* sides of the pair fail to bind and the pair reports `Error` — it is
//! never even tried. Defining the name as a macro turns those pairs from "not attempted" into
//! "attempted", which is the whole purpose of this module.
//!
//! # What may go in here, and what may not
//!
//! The tester is a *disprover*, so the failure it must not have is a false `NOT-EQUIVALENT`, and a
//! shim is a new channel for exactly that. The bar is an **exact** match of the Postgres
//! semantics, not an approximation that is usually right.
//!
//! Applying the same macro to both sides is not on its own enough to make a loose mapping safe. It
//! rules out one failure — a macro cannot split two sides that feed it equal values — but not the
//! one that matters: if A computes `f(x)` and B computes `f(y)` where Postgres has `f(x) = f(y)`,
//! a macro mapping `x` and `y` to *different* values refutes a pair Postgres calls equivalent.
//! `initcap` is the worked example: Postgres folds `'foo bar'` and `'FOO BAR'` to one string, so a
//! shim that does not fold them too manufactures counterexamples.
//!
//! The inverse error is safe but costly: a mapping that collapses values Postgres keeps apart only
//! loses refutations we would otherwise make. That asymmetry is why anything needing a real
//! translation rather than a rename is left out. Postgres `to_char` format strings are not
//! `strftime`'s, `make_interval`'s arguments are named and mapping them is a translation rather
//! than a rename, `regexp_match` returns capture groups rather than the match, and the trigram,
//! full-text and jsonpath families are feature areas rather than names. Those pairs keep
//! reporting `Error`, which is the honest answer.
//!
//! # Where Postgres raises, the shim must raise
//!
//! A mapping can be exact on every value Postgres accepts and still be a false-refutation channel,
//! because *refusing* an input is part of the semantics. Postgres raises "cannot extract elements
//! from a scalar" for `json_array_elements` of a non-array; DuckDB's `json_extract(j, '$[*]')`
//! quietly answers `[]`, and `json_keys` and `json_array_length` are the same. A raise makes
//! [`crate::pair`] skip the trial, exactly as it would for any other error, so the two sides are
//! never compared. An empty answer instead *drops a row*, and a pair whose two sides differ only
//! in how they treat a dropped row is then refuted on an input Postgres would have rejected.
//!
//! So every set-returning entry, and `jsonb_array_length`, checks `json_type` and raises through
//! `error()` on anything Postgres would refuse. A SQL NULL is separate and is passed through: for
//! a strict set-returning function Postgres really does answer with no rows.
//!
//! Two limits of this, both checked against DuckDB rather than assumed. A macro **cannot** guard a
//! function DuckDB already has, because a body that calls the name it defines recurses until the
//! binder's expression-depth limit — so native `json_array_length` keeps DuckDB's permissive
//! answer, and a pair that calls it on a non-array can still be refuted on an input Postgres
//! rejects. And a pair mixing the two spellings gets the guarded one's raise, which skips the trial
//! rather than manufacturing anything.
//!
//! # Position
//!
//! Postgres's set-returning functions are legal both in the select list and in `FROM`. A scalar
//! macro over `unnest` serves the first, a `TABLE` macro serves the second, and DuckDB keeps the
//! two in separate namespaces — so a set-returning entry below defines both under one name. The
//! `TABLE` form's output column is named the way Postgres names it, which is `value` where the
//! function has that `OUT` parameter and the function's own name where it does not.
//!
//! What no macro can serve is `DISTINCT`, `ORDER BY` or `FILTER` *inside* the call:
//! `jsonb_object_agg(k, v ORDER BY t)` is rejected with "is a Macro Function" whatever the body
//! is. Those call sites keep erroring.
//!
//! # Installation
//!
//! [`install`] defines only the macros whose name occurs in the pair's own SQL, because the scan
//! is free and defining all of them on every pair is not. The match is a case-insensitive
//! substring test, deliberately over-eager: a column named `x_initcap_y` installs a macro nothing
//! calls, which costs nothing, where a miss would leave the pair erroring. None of these names
//! exist in DuckDB, so an unused macro cannot shadow a builtin.
//!
//! Every statement is `CREATE OR REPLACE`. A bare `CREATE MACRO` would start failing with "already
//! exists" if a later DuckDB gained one of these names natively, and the shim would then silently
//! stop installing the rest. Replacing a native name is safe, but only because no body below calls
//! the name it defines — that would recurse rather than reach the builtin, and a test pins it.

use duckdb::Connection;

/// DuckDB's `json_type` answers in SQL type names (`VARCHAR`, `UBIGINT`, `DOUBLE`) where Postgres
/// answers in JSON ones (`string`, `number`), so this one is a map rather than an alias. The map is
/// explicit and total: an unrecognised spelling yields NULL rather than a guess, and the `ELSE`
/// is also what keeps a SQL NULL input from falling through to `'number'`.
macro_rules! json_typeof_ddl {
    ($name:literal) => {
        concat!(
            "CREATE OR REPLACE MACRO ",
            $name,
            "(j) AS CASE json_type(j) \
             WHEN 'OBJECT' THEN 'object' \
             WHEN 'ARRAY' THEN 'array' \
             WHEN 'VARCHAR' THEN 'string' \
             WHEN 'BOOLEAN' THEN 'boolean' \
             WHEN 'NULL' THEN 'null' \
             WHEN 'UBIGINT' THEN 'number' \
             WHEN 'BIGINT' THEN 'number' \
             WHEN 'DOUBLE' THEN 'number' \
             ELSE NULL END"
        )
    };
}

/// `json_object` will not take a macro parameter as a key without an explicit cast, and it rejects
/// the macro at *creation* time rather than at the call, so the cast is not optional. Postgres
/// accepts any even arity; the overloads here stop at seven key–value pairs, and a wider call
/// simply keeps erroring as it does today. Odd arities are absent because Postgres rejects
/// them too.
macro_rules! build_object_ddl {
    ($name:literal) => {
        concat!(
            "CREATE OR REPLACE MACRO ", $name,
            "(k1,v1) AS json_object(CAST(k1 AS VARCHAR),v1)",
            ", (k1,v1,k2,v2) AS json_object(CAST(k1 AS VARCHAR),v1,CAST(k2 AS VARCHAR),v2)",
            ", (k1,v1,k2,v2,k3,v3) AS json_object(CAST(k1 AS VARCHAR),v1,CAST(k2 AS VARCHAR),v2,CAST(k3 AS VARCHAR),v3)",
            ", (k1,v1,k2,v2,k3,v3,k4,v4) AS json_object(CAST(k1 AS VARCHAR),v1,CAST(k2 AS VARCHAR),v2,CAST(k3 AS VARCHAR),v3,CAST(k4 AS VARCHAR),v4)",
            ", (k1,v1,k2,v2,k3,v3,k4,v4,k5,v5) AS json_object(CAST(k1 AS VARCHAR),v1,CAST(k2 AS VARCHAR),v2,CAST(k3 AS VARCHAR),v3,CAST(k4 AS VARCHAR),v4,CAST(k5 AS VARCHAR),v5)",
            ", (k1,v1,k2,v2,k3,v3,k4,v4,k5,v5,k6,v6) AS json_object(CAST(k1 AS VARCHAR),v1,CAST(k2 AS VARCHAR),v2,CAST(k3 AS VARCHAR),v3,CAST(k4 AS VARCHAR),v4,CAST(k5 AS VARCHAR),v5,CAST(k6 AS VARCHAR),v6)",
            ", (k1,v1,k2,v2,k3,v3,k4,v4,k5,v5,k6,v6,k7,v7) AS json_object(CAST(k1 AS VARCHAR),v1,CAST(k2 AS VARCHAR),v2,CAST(k3 AS VARCHAR),v3,CAST(k4 AS VARCHAR),v4,CAST(k5 AS VARCHAR),v5,CAST(k6 AS VARCHAR),v6,CAST(k7 AS VARCHAR),v7)"
        )
    };
}

/// A path lookup that misses is NULL in Postgres rather than an error, which
/// `json_extract_string` already matches.
macro_rules! extract_path_text_ddl {
    ($name:literal) => {
        concat!(
            "CREATE OR REPLACE MACRO ", $name,
            "(j,a) AS json_extract_string(j,'$.'||a)",
            ", (j,a,b) AS json_extract_string(j,'$.'||a||'.'||b)",
            ", (j,a,b,c) AS json_extract_string(j,'$.'||a||'.'||b||'.'||c)"
        )
    };
}

/// Scalar form for the select list and `TABLE` form for `FROM`, under one name. `$col` is the
/// column name Postgres gives the `FROM` form.
/// A set-returning entry, in both call positions, refusing the inputs Postgres refuses.
///
/// `$jtype` is the `json_type` spelling this function accepts; anything else raises through
/// `error()`, because that is what Postgres does and DuckDB does not — see the module docs. A SQL
/// NULL is checked first and passed through as an empty result, which *is* Postgres's answer for a
/// strict set-returning function. Both branches are cast to `$listty` so the `CASE` has one type;
/// `error()` sits under a cast for the same reason.
macro_rules! setof_ddl {
    ($name:literal, $jtype:literal, $expr:literal, $listty:literal, $err:literal, $col:literal) => {
        setof_ddl!(@both $name, concat!(
            "unnest(CASE WHEN j IS NULL THEN CAST(NULL AS ", $listty, ")",
            " WHEN json_type(j) = '", $jtype, "' THEN ", $expr,
            " ELSE CAST(error('", $err, "') AS ", $listty, ") END)"
        ), $col)
    };
    (@both $name:literal, $body:expr, $col:literal) => {
        concat!(
            "CREATE OR REPLACE MACRO ", $name, "(j) AS ", $body, ";\n",
            "CREATE OR REPLACE MACRO ", $name, "(j) AS TABLE SELECT ", $body, " AS ", $col
        )
    };
}

/// `jsonb_build_object` is not `json_build_object`: a `jsonb` object stores its keys in its own
/// order -- shorter keys first, then bytewise -- and keeps only the last of duplicate keys, so
/// `jsonb_build_object('a', 1, 'b', 2)` and `('b', 2, 'a', 1)` are one value, and so are
/// `('a', 1, 'a', 2)` and `('a', 2)`. DuckDB's `json_object` keeps both the call's order and its
/// duplicates, which is right for the `json` builder and refutes equivalent pairs for this one.
///
/// So the arguments are gathered into a key list and a value list, the last occurrence of each key is
/// kept, the survivors are sorted by (length, key) and the object is written out from them -- in
/// `json_object`'s own spelling, so it compares as text with the objects the other shims build. A
/// NULL key raises, as in Postgres (`argument … key must not be null`); a NULL value is a JSON
/// `null`. The arities stop at seven pairs, like `json_build_object`'s.
const JSONB_BUILD_OBJECT_DDL: &str = concat!(
    "CREATE OR REPLACE MACRO sqleq_jsonb_object(ks, vs) AS ",
    "CASE WHEN len(list_filter(ks, x -> x IS NULL)) > 0 ",
    "THEN CAST(error('argument of jsonb_build_object: key must not be null') AS JSON) ",
    "ELSE CAST('{' || array_to_string(list_transform(list_sort(list_transform(",
    "list_filter(range(len(ks)), i -> NOT list_contains(ks[i + 2:], ks[i + 1])), ",
    "i -> {'l': length(ks[i + 1]), 'k': ks[i + 1], 'v': vs[i + 1]})), ",
    "s -> CAST(to_json(s.k) AS VARCHAR) || ':' || CAST(s.v AS VARCHAR)), ',') || '}' AS JSON) END;\n",
    "CREATE OR REPLACE MACRO jsonb_build_object",
            "(k1,v1) AS sqleq_jsonb_object([CAST(k1 AS VARCHAR)], [coalesce(to_json(v1), 'null'::JSON)])",
            ", (k1,v1,k2,v2) AS sqleq_jsonb_object([CAST(k1 AS VARCHAR), CAST(k2 AS VARCHAR)], [coalesce(to_json(v1), 'null'::JSON), coalesce(to_json(v2), 'null'::JSON)])",
            ", (k1,v1,k2,v2,k3,v3) AS sqleq_jsonb_object([CAST(k1 AS VARCHAR), CAST(k2 AS VARCHAR), CAST(k3 AS VARCHAR)], [coalesce(to_json(v1), 'null'::JSON), coalesce(to_json(v2), 'null'::JSON), coalesce(to_json(v3), 'null'::JSON)])",
            ", (k1,v1,k2,v2,k3,v3,k4,v4) AS sqleq_jsonb_object([CAST(k1 AS VARCHAR), CAST(k2 AS VARCHAR), CAST(k3 AS VARCHAR), CAST(k4 AS VARCHAR)], [coalesce(to_json(v1), 'null'::JSON), coalesce(to_json(v2), 'null'::JSON), coalesce(to_json(v3), 'null'::JSON), coalesce(to_json(v4), 'null'::JSON)])",
            ", (k1,v1,k2,v2,k3,v3,k4,v4,k5,v5) AS sqleq_jsonb_object([CAST(k1 AS VARCHAR), CAST(k2 AS VARCHAR), CAST(k3 AS VARCHAR), CAST(k4 AS VARCHAR), CAST(k5 AS VARCHAR)], [coalesce(to_json(v1), 'null'::JSON), coalesce(to_json(v2), 'null'::JSON), coalesce(to_json(v3), 'null'::JSON), coalesce(to_json(v4), 'null'::JSON), coalesce(to_json(v5), 'null'::JSON)])",
            ", (k1,v1,k2,v2,k3,v3,k4,v4,k5,v5,k6,v6) AS sqleq_jsonb_object([CAST(k1 AS VARCHAR), CAST(k2 AS VARCHAR), CAST(k3 AS VARCHAR), CAST(k4 AS VARCHAR), CAST(k5 AS VARCHAR), CAST(k6 AS VARCHAR)], [coalesce(to_json(v1), 'null'::JSON), coalesce(to_json(v2), 'null'::JSON), coalesce(to_json(v3), 'null'::JSON), coalesce(to_json(v4), 'null'::JSON), coalesce(to_json(v5), 'null'::JSON), coalesce(to_json(v6), 'null'::JSON)])",
            ", (k1,v1,k2,v2,k3,v3,k4,v4,k5,v5,k6,v6,k7,v7) AS sqleq_jsonb_object([CAST(k1 AS VARCHAR), CAST(k2 AS VARCHAR), CAST(k3 AS VARCHAR), CAST(k4 AS VARCHAR), CAST(k5 AS VARCHAR), CAST(k6 AS VARCHAR), CAST(k7 AS VARCHAR)], [coalesce(to_json(v1), 'null'::JSON), coalesce(to_json(v2), 'null'::JSON), coalesce(to_json(v3), 'null'::JSON), coalesce(to_json(v4), 'null'::JSON), coalesce(to_json(v5), 'null'::JSON), coalesce(to_json(v6), 'null'::JSON), coalesce(to_json(v7), 'null'::JSON)])"
);

/// A divisor that raises on zero, as Postgres's `/`, `%` and `mod()` do, and is otherwise its
/// argument, type included -- `error()` is untyped, so the `CASE` takes the argument's type and an
/// integer division stays one. `crate::rewrite::postgres_operators` wraps every divisor that is not a
/// non-zero literal in it.
const NONZERO_DDL: &str = "CREATE OR REPLACE MACRO sqleq_nonzero(x) AS \
     CASE WHEN x = 0 THEN error('division by zero') ELSE x END";

/// A pattern that DuckDB's full-match `~` finds anywhere in the string, as Postgres's `~` does: a
/// full match of `(?s).*(?:p).*` is a match of `p` anywhere. `(?s)` lets `.` match a newline, which
/// it does in Postgres's default (not newline-sensitive) mode, and `^`/`$` still anchor at the ends of
/// the string only. The `i` variant is `~*`'s.
const PARTIAL_DDL: &str =
    "CREATE OR REPLACE MACRO sqleq_partial(p) AS '(?s).*(?:' || p || ').*'";
const IPARTIAL_DDL: &str =
    "CREATE OR REPLACE MACRO sqleq_ipartial(p) AS '(?si).*(?:' || p || ').*'";

/// `power` and `exp` over `double precision`, raising where Postgres's `dpow` and `dexp` raise and
/// DuckDB answers `inf`, `NaN` or `0`: a zero base with a negative exponent, a negative base with a
/// fractional one, and a finite result Postgres reports as an overflow (infinite) or an underflow
/// (zero from a non-zero base). They also raise on a non-finite argument and on a subnormal result,
/// where what Postgres does depends on the platform's `pow` and `exp`; a raise only ever skips the
/// trial. `crate::rewrite::postgres_operators` renames `power`, `pow` and `exp` calls to these (a
/// macro cannot take a name DuckDB has), and `crate::pgtype::unmodelled` withholds the `numeric`
/// ones before that.
const POWER_DDL: &str = r"CREATE OR REPLACE MACRO sqleq_power_d(x, y) AS
    CASE WHEN isnan(x) OR isinf(x) OR isnan(y) OR isinf(y)
              THEN error('power of a non-finite value')
         WHEN x = 0 AND y < 0 THEN error('zero raised to a negative power is undefined')
         WHEN x < 0 AND floor(y) <> y
              THEN error('a negative number raised to a non-integer power yields a complex result')
         WHEN isinf(pow(x, y)) THEN error('value out of range: overflow')
         WHEN pow(x, y) = 0 AND x <> 0 THEN error('value out of range: underflow')
         WHEN pow(x, y) <> 0 AND abs(pow(x, y)) < 2.2250738585072014e-308
              THEN error('a subnormal power')
         ELSE pow(x, y) END;
CREATE OR REPLACE MACRO sqleq_power(x, y) AS sqleq_power_d(CAST(x AS DOUBLE), CAST(y AS DOUBLE))";
const EXP_DDL: &str = r"CREATE OR REPLACE MACRO sqleq_exp_d(x) AS
    CASE WHEN isnan(x) OR isinf(x) THEN error('exp of a non-finite value')
         WHEN isinf(exp(x)) THEN error('value out of range: overflow')
         WHEN exp(x) = 0 THEN error('value out of range: underflow')
         WHEN exp(x) < 2.2250738585072014e-308 THEN error('a subnormal exp')
         ELSE exp(x) END;
CREATE OR REPLACE MACRO sqleq_exp(x) AS sqleq_exp_d(CAST(x AS DOUBLE))";

/// DuckDB has no `initcap` at all, so this is the one entry that is an implementation rather than
/// a rename. Postgres uppercases the first character of each word and lowercases the rest, where a
/// word is a run of alphanumerics; the character-wise form below states exactly that. The class is
/// `[\p{L}\p{N}]`, not `[A-Za-z0-9]` — Postgres's notion of alphanumeric is locale-aware, and an
/// ASCII-only class turns `'Ünicode'` into `'ÜNicode'`.
const INITCAP_DDL: &str = r"CREATE OR REPLACE MACRO initcap(s) AS
    CASE WHEN s IS NULL THEN NULL ELSE
      array_to_string(
        list_transform(
          generate_series(1, length(s)),
          i -> CASE WHEN i = 1 OR NOT regexp_matches(substr(s, i - 1, 1), '[\p{L}\p{N}]')
                    THEN upper(substr(s, i, 1))
                    ELSE lower(substr(s, i, 1)) END),
        '')
    END";

/// `(name as it appears in the SQL, the DDL defining it)`. One entry may define several arities or
/// both call positions; either way it is one `execute_batch`.
static SHIMS: &[(&str, &str)] = &[
    // -- aggregates ---------------------------------------------------------------------------
    // DuckDB's `json_group_array` already matches `json_agg` on the two edges that usually differ:
    // no input rows gives SQL NULL rather than `'[]'`, and a NULL input is kept as a JSON `null`
    // rather than skipped. Both are pinned in the tests, because a change to either would turn
    // this alias into a false-refutation channel silently.
    ("json_agg", "CREATE OR REPLACE MACRO json_agg(x) AS json_group_array(x)"),
    ("jsonb_agg", "CREATE OR REPLACE MACRO jsonb_agg(x) AS json_group_array(x)"),
    (
        "json_object_agg",
        "CREATE OR REPLACE MACRO json_object_agg(k,v) AS json_group_object(k,v)",
    ),
    (
        "jsonb_object_agg",
        "CREATE OR REPLACE MACRO jsonb_object_agg(k,v) AS json_group_object(k,v)",
    ),
    // -- scalar renames -----------------------------------------------------------------------
    (
        "jsonb_array_length",
        concat!(
            "CREATE OR REPLACE MACRO jsonb_array_length(j) AS CASE WHEN j IS NULL THEN NULL",
            " WHEN json_type(j) = 'ARRAY' THEN CAST(json_array_length(j) AS BIGINT)",
            " ELSE CAST(error('cannot get array length of a non-array') AS BIGINT) END"
        ),
    ),
    ("to_jsonb", "CREATE OR REPLACE MACRO to_jsonb(x) AS to_json(x)"),
    // One statement with two arities: DuckDB rejects a second `CREATE MACRO` for a name it already
    // has, so the overloads cannot be added one at a time.
    ("btrim", "CREATE OR REPLACE MACRO btrim(s) AS trim(s), (s,c) AS trim(s,c)"),
    // Postgres returns the upper *subscript*, which for the 1-based one-dimensional arrays this
    // tester generates is the length — except on an empty array, where Postgres gives NULL and
    // `len` gives 0. Any dimension other than 1 is NULL for the same reason.
    (
        "array_upper",
        "CREATE OR REPLACE MACRO array_upper(a,d) AS CASE WHEN d = 1 AND len(a) > 0 THEN len(a) ELSE NULL END",
    ),
    // `IS DISTINCT FROM`, not `<>`: Postgres removes the NULL elements when the value to remove is
    // NULL, which a `<>` filter would drop on the floor because NULL is not true.
    (
        "array_remove",
        "CREATE OR REPLACE MACRO array_remove(a,e) AS list_filter(a, x -> x IS DISTINCT FROM e)",
    ),
    ("json_typeof", json_typeof_ddl!("json_typeof")),
    ("jsonb_typeof", json_typeof_ddl!("jsonb_typeof")),
    // -- set-returning ---------------------------------------------------------------------------
    (
        "json_array_elements",
        setof_ddl!(
            "json_array_elements", "ARRAY", "json_extract(j,'$[*]')", "JSON[]", "cannot extract elements from a non-array", "value"
        ),
    ),
    (
        "jsonb_array_elements",
        setof_ddl!(
            "jsonb_array_elements", "ARRAY", "json_extract(j,'$[*]')", "JSON[]", "cannot extract elements from a non-array", "value"
        ),
    ),
    (
        "json_array_elements_text",
        setof_ddl!(
            "json_array_elements_text", "ARRAY", "json_extract_string(j,'$[*]')", "VARCHAR[]",
            "cannot extract elements from a non-array", "value"
        ),
    ),
    (
        "jsonb_array_elements_text",
        setof_ddl!(
            "jsonb_array_elements_text", "ARRAY", "json_extract_string(j,'$[*]')", "VARCHAR[]",
            "cannot extract elements from a non-array", "value"
        ),
    ),
    // No `OUT` parameter on this one in Postgres, so the `FROM` column takes the function's name.
    (
        "json_object_keys",
        setof_ddl!(
            "json_object_keys", "OBJECT", "json_keys(j)", "VARCHAR[]", "cannot call json_object_keys on a non-object", "json_object_keys"
        ),
    ),
    (
        "jsonb_object_keys",
        setof_ddl!(
            "jsonb_object_keys", "OBJECT", "json_keys(j)", "VARCHAR[]", "cannot call json_object_keys on a non-object",
            "jsonb_object_keys"
        ),
    ),
    // -- builders and paths ----------------------------------------------------------------------
    ("json_build_object", build_object_ddl!("json_build_object")),
    ("jsonb_build_object", JSONB_BUILD_OBJECT_DDL),
    ("json_extract_path_text", extract_path_text_ddl!("json_extract_path_text")),
    ("jsonb_extract_path_text", extract_path_text_ddl!("jsonb_extract_path_text")),
    // -- the one implementation ------------------------------------------------------------------
    ("initcap", INITCAP_DDL),
    // -- Postgres operator semantics, named by `crate::rewrite::postgres_operators` ---------------
    ("sqleq_nonzero", NONZERO_DDL),
    ("sqleq_partial", PARTIAL_DDL),
    ("sqleq_ipartial", IPARTIAL_DDL),
    ("sqleq_power", POWER_DDL),
    ("sqleq_exp", EXP_DDL),
];

/// Define every shimmed function whose name appears in `sqls`.
///
/// The scan is over the pair's raw text, so a name inside a string literal or a column name also
/// triggers its macro; the module docs say why that direction is the safe one. Returns on the
/// first definition error rather than continuing: a macro that fails to define is a defect in the
/// table above, and the pair it was installing for would otherwise report a confusing bind error
/// in its place.
pub fn install(con: &Connection, sqls: &[&str]) -> duckdb::Result<()> {
    let hay: Vec<String> = sqls.iter().map(|s| s.to_ascii_lowercase()).collect();
    for (name, ddl) in SHIMS {
        if hay.iter().any(|h| h.contains(name)) {
            con.execute_batch(ddl)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{install, SHIMS};
    use crate::duck::open_db;
    use duckdb::Connection;

    /// A connection with every shim installed, obtained by handing `install` a string that names
    /// them all. Tests about *selective* installation build their own connection instead.
    fn shimmed() -> Connection {
        let all: String = SHIMS.iter().map(|(n, _)| format!("{n} ")).collect();
        let con = open_db().unwrap();
        install(&con, &[&all]).unwrap();
        con
    }

    fn text(con: &Connection, q: &str) -> Option<String> {
        con.query_row(q, [], |r| r.get::<_, Option<String>>(0)).unwrap()
    }

    fn texts(con: &Connection, q: &str) -> Vec<Option<String>> {
        con.prepare(q)
            .unwrap()
            .query_map([], |r| r.get::<_, Option<String>>(0))
            .unwrap()
            .collect::<duckdb::Result<_>>()
            .unwrap()
    }

    #[test]
    fn every_shim_defines_cleanly() {
        // `install` returns on the first error, so this covers the whole table at once: a syntax
        // error, or a body naming something DuckDB does not have, fails here rather than mid-run.
        shimmed();
    }

    /// The `json_agg` alias rests on two DuckDB behaviours that happen to match Postgres. Either
    /// changing makes the alias wrong in the direction that manufactures counterexamples, so both
    /// are pinned rather than assumed.
    #[test]
    fn json_agg_matches_postgres_on_empty_input_and_on_nulls() {
        let con = shimmed();
        assert_eq!(
            text(&con, "SELECT jsonb_agg(v) FROM (SELECT 1 v) WHERE false"),
            None,
            "json_agg over no rows is NULL in Postgres, not '[]'"
        );
        assert_eq!(
            text(&con, "SELECT jsonb_agg(v) FROM (VALUES (1),(NULL),(3)) s(v)").as_deref(),
            Some("[1,null,3]"),
            "json_agg keeps a NULL input as JSON null"
        );
        assert_eq!(
            text(&con, "SELECT json_agg(v) FROM (VALUES ('a'),('b')) s(v)").as_deref(),
            Some(r#"["a","b"]"#)
        );
        assert_eq!(
            text(&con, "SELECT jsonb_object_agg(k,v) FROM (VALUES ('a',1),('b',2)) s(k,v)")
                .as_deref(),
            Some(r#"{"a":1,"b":2}"#)
        );
    }

    #[test]
    fn json_typeof_returns_the_postgres_spellings() {
        let con = shimmed();
        for (doc, want) in [
            ("{}", "object"),
            ("[]", "array"),
            (r#""s""#, "string"),
            ("1", "number"),
            ("-5", "number"),
            ("1.5", "number"),
            ("true", "boolean"),
            ("null", "null"),
        ] {
            let got = text(&con, &format!("SELECT jsonb_typeof('{doc}'::JSON)"));
            assert_eq!(got.as_deref(), Some(want), "jsonb_typeof({doc})");
        }
        // A SQL NULL input stays NULL rather than falling through the CASE to 'number'.
        assert_eq!(text(&con, "SELECT json_typeof(NULL::JSON)"), None);
    }

    /// The case this module is riskiest for: Postgres folds these two to one string, so a shim
    /// that does not fold them refutes pairs Postgres calls equivalent.
    #[test]
    fn initcap_folds_case_the_way_postgres_does() {
        let con = shimmed();
        let folds = con
            .query_row("SELECT initcap('foo bar') = initcap('FOO BAR')", [], |r| {
                r.get::<_, bool>(0)
            })
            .unwrap();
        assert!(folds, "initcap must fold case, or it manufactures counterexamples");

        for (input, want) in [
            ("hello world", "Hello World"),
            ("HELLO WORLD", "Hello World"),
            ("hI tHERE", "Hi There"),
            ("o''neil mc-do", "O'Neil Mc-Do"),
            ("a1b c", "A1b C"),
            ("123abc", "123abc"),
            ("  x", "  X"),
            ("", ""),
            // An ASCII-only alphanumeric class gets this one wrong, as 'ÜNicode'.
            ("Ünicode ümlaut", "Ünicode Ümlaut"),
        ] {
            let got = text(&con, &format!("SELECT initcap('{input}')"));
            assert_eq!(got.as_deref(), Some(want), "initcap({input:?})");
        }
        assert_eq!(text(&con, "SELECT initcap(NULL)"), None);
    }

    #[test]
    fn array_helpers_match_the_postgres_edges() {
        let con = shimmed();
        let int = |q: &str| con.query_row(q, [], |r| r.get::<_, Option<i64>>(0)).unwrap();
        // Postgres `array_upper` is NULL on an empty array and on a dimension the array lacks;
        // only `len` would answer 0.
        assert_eq!(int("SELECT array_upper([1,2,3],1)"), Some(3));
        assert_eq!(int("SELECT array_upper([]::INT[],1)"), None);
        assert_eq!(int("SELECT array_upper([1,2],2)"), None);
        // Removing 2 leaves the NULL element in place, and removing NULL removes it — an `x <> e`
        // filter would get both of these wrong.
        assert_eq!(int("SELECT len(array_remove([1,2,NULL,2,3],2))"), Some(3));
        assert_eq!(int("SELECT len(array_remove([1,NULL,2],NULL))"), Some(2));
    }

    #[test]
    fn scalar_renames_bind_and_agree() {
        let con = shimmed();
        assert_eq!(
            con.query_row("SELECT jsonb_array_length('[1,2,3]')", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            3
        );
        assert_eq!(text(&con, "SELECT to_jsonb(42)").as_deref(), Some("42"));
        assert_eq!(text(&con, "SELECT btrim('  x  ')").as_deref(), Some("x"));
        assert_eq!(text(&con, "SELECT btrim('xxaxx','x')").as_deref(), Some("a"));
        assert_eq!(
            text(&con, r#"SELECT jsonb_extract_path_text('{"a":"v"}','a')"#).as_deref(),
            Some("v")
        );
        // A path that is not there is NULL in Postgres, not an error.
        assert_eq!(text(&con, r#"SELECT jsonb_extract_path_text('{"a":"v"}','zz')"#), None);
        assert_eq!(
            text(&con, "SELECT jsonb_build_object('a',1,'b','x')").as_deref(),
            Some(r#"{"a":1,"b":"x"}"#)
        );
    }

    /// `jsonb` normalizes an object: keys by (length, bytes), the last duplicate kept. The `json`
    /// builder keeps what it was given, as Postgres's does.
    #[test]
    fn jsonb_build_object_normalizes_like_jsonb() {
        let con = shimmed();
        for (q, want) in [
            ("jsonb_build_object('b', 2, 'a', 1)", r#"{"a":1,"b":2}"#),
            ("jsonb_build_object('aa', 1, 'b', 2)", r#"{"b":2,"aa":1}"#),
            ("jsonb_build_object('a', 1, 'a', 2)", r#"{"a":2}"#),
            ("jsonb_build_object('a', NULL)", r#"{"a":null}"#),
            ("jsonb_build_object(1, true)", r#"{"1":true}"#),
            ("json_build_object('b', 2, 'a', 1)", r#"{"b":2,"a":1}"#),
        ] {
            assert_eq!(text(&con, &format!("SELECT {q}")).as_deref(), Some(want), "{q}");
        }
        assert!(con.execute_batch("SELECT jsonb_build_object(NULL, 1)").is_err());
    }

    /// The divisor guard raises on a zero of every numeric type and is the identity otherwise, type
    /// included, so integer division stays integer division.
    #[test]
    fn the_divisor_guard_raises_on_zero_and_keeps_the_type() {
        let con = shimmed();
        for q in [
            "SELECT 1 / sqleq_nonzero(0)",
            "SELECT 1.0 / sqleq_nonzero(0.0)",
            "SELECT 1 / sqleq_nonzero(CAST(-0.0 AS DOUBLE))",
            "SELECT 5 % sqleq_nonzero(0)",
        ] {
            assert!(con.execute_batch(q).is_err(), "must raise: {q}");
        }
        assert_eq!(text(&con, "SELECT typeof(7 / sqleq_nonzero(2))").as_deref(), Some("INTEGER"));
        assert_eq!(text(&con, "SELECT CAST(7 / sqleq_nonzero(2) AS VARCHAR)").as_deref(), Some("3"));
        assert_eq!(text(&con, "SELECT CAST(7 / sqleq_nonzero(NULL) AS VARCHAR)"), None);
    }

    /// A partial match anywhere, as Postgres's `~`, against DuckDB's full match.
    #[test]
    fn the_partial_match_finds_a_pattern_anywhere() {
        let con = shimmed();
        let b = |q: &str| con.query_row(q, [], |r| r.get::<_, Option<bool>>(0)).unwrap();
        assert_eq!(b("SELECT 'ax' ~ 'a'"), Some(false), "DuckDB's own `~` is a full match");
        assert_eq!(b("SELECT 'ax' ~ sqleq_partial('a')"), Some(true));
        assert_eq!(b("SELECT 'xa' ~ sqleq_partial('^a')"), Some(false));
        assert_eq!(b("SELECT 'a|b' ~ sqleq_partial('x|b')"), Some(true));
        assert_eq!(b("SELECT 'xy\na' ~ sqleq_partial('y.a')"), Some(true), "`.` matches a newline");
        assert_eq!(b("SELECT 'x\nay' ~ sqleq_partial('^a')"), Some(false), "`^` is the string's start");
        assert_eq!(b("SELECT 'AX' ~ sqleq_ipartial('a')"), Some(true));
        assert_eq!(b("SELECT 'ax' ~ sqleq_partial(NULL)"), None);
    }

    /// Set-returning functions are legal in both positions, so both forms are defined and both have
    /// to work from the one connection that defined them.
    #[test]
    fn set_returning_shims_work_in_the_select_list_and_in_from() {
        let con = shimmed();
        assert_eq!(
            texts(&con, r#"SELECT jsonb_array_elements('[1,{"a":2},3]')"#),
            [Some("1".into()), Some(r#"{"a":2}"#.into()), Some("3".into())]
        );
        assert_eq!(
            texts(&con, "SELECT value FROM jsonb_array_elements('[1,2]')"),
            [Some("1".into()), Some("2".into())],
            "the TABLE form has to survive the scalar one"
        );
        assert_eq!(
            texts(&con, r#"SELECT jsonb_array_elements_text('[1,"b",null]')"#),
            [Some("1".into()), Some("b".into()), None]
        );
        assert_eq!(
            texts(&con, r#"SELECT jsonb_object_keys('{"a":1,"b":2}')"#),
            [Some("a".into()), Some("b".into())]
        );
        assert_eq!(
            texts(&con, r#"SELECT jsonb_object_keys FROM jsonb_object_keys('{"a":1}')"#),
            [Some("a".into())],
            "Postgres names this column after the function, having no OUT parameter"
        );
    }

    /// The control for every test above: they would all pass unchanged if `install` ignored its
    /// argument and defined the whole table every time.
    /// Postgres refuses a non-array to `json_array_elements` and a non-object to
    /// `json_object_keys`, and refusing is part of the semantics: a raise skips the trial, where
    /// DuckDB's own `[]` would drop a row and let a pair be refuted on an input Postgres rejects.
    #[test]
    fn the_shims_raise_on_the_inputs_postgres_refuses() {
        let con = shimmed();
        for q in [
            r#"SELECT jsonb_array_elements('"c"')"#,
            r#"SELECT jsonb_array_elements('{"a":1}')"#,
            r#"SELECT value FROM jsonb_array_elements('3')"#,
            r#"SELECT json_array_elements_text('{"a":1}')"#,
            r#"SELECT jsonb_object_keys('[1,2]')"#,
            r#"SELECT jsonb_object_keys('"c"')"#,
            r#"SELECT jsonb_array_length('{"a":1}')"#,
        ] {
            assert!(con.execute_batch(q).is_err(), "must not answer quietly: {q}");
        }

        // An empty collection is not a refusal, and neither is a SQL NULL: Postgres answers both
        // with no rows. Keeping these apart from the raise above is the whole point of the guard.
        assert!(texts(&con, "SELECT jsonb_array_elements('[]')").is_empty());
        assert!(texts(&con, "SELECT jsonb_object_keys('{}')").is_empty());
        let len = |q: &str| con.query_row(q, [], |r| r.get::<_, Option<i64>>(0)).unwrap();
        assert_eq!(len("SELECT jsonb_array_length('[]')"), Some(0));
        assert_eq!(len("SELECT jsonb_array_length(NULL)"), None);

        // Through a column rather than a literal, which is the shape a real pair has, and with a
        // NULL row among the good ones so the guard is exercised per row and not folded away.
        con.execute_batch(
            r#"CREATE TABLE t(id INT, j JSON);
               INSERT INTO t VALUES (1,'[1,2]'), (2,NULL), (3,'[]')"#,
        )
        .unwrap();
        assert_eq!(
            texts(&con, "SELECT jsonb_array_elements(j) FROM t ORDER BY id"),
            [Some("1".into()), Some("2".into())],
            "a strict set-returning function drops the NULL and the empty row"
        );
        con.execute_batch(r#"INSERT INTO t VALUES (4,'{"a":1}')"#).unwrap();
        assert!(con.execute_batch("SELECT jsonb_array_elements(j) FROM t").is_err());
    }

    /// A macro body that calls the name it defines does not reach the builtin underneath — it
    /// recurses until the binder's expression-depth limit, which is why native `json_array_length`
    /// cannot be guarded this way. Nothing in the table does it today; this is what keeps it so.
    #[test]
    fn no_shim_body_calls_the_name_it_defines() {
        // One statement at a time, and only the text after its *first* ` AS `: a whole-DDL split
        // would read the `TABLE` statement's own header as part of the scalar statement's body.
        let bodies = |ddl: &'static str, name: &str| -> Vec<String> {
            ddl.split(';')
                .filter_map(|stmt| stmt.split_once(" AS ").map(|(_, b)| b.to_string()))
                .filter(|b| b.contains(&format!("{name}(")))
                .collect()
        };
        for (name, ddl) in SHIMS {
            assert!(
                bodies(ddl, name).is_empty(),
                "{name} would recurse rather than reach a builtin"
            );
        }
        // Non-vacuity, in the shape a real entry has, or the check above passes by reading nothing.
        let bad = "CREATE OR REPLACE MACRO json_array_length(j) AS json_array_length(j);\n\
                   CREATE OR REPLACE MACRO json_array_length(j) AS TABLE SELECT 1 AS v";
        assert_eq!(bodies(bad, "json_array_length").len(), 1);
    }

    #[test]
    fn nothing_is_installed_for_sql_that_names_no_shimmed_function() {
        let sql = "SELECT id, name FROM t WHERE x = $1";

        let con = open_db().unwrap();
        install(&con, &[sql]).unwrap();
        assert!(
            con.execute_batch("SELECT jsonb_agg(1)").is_err(),
            "jsonb_agg must still be undefined when the pair never mentions it"
        );

        // ... and the same connection gains it as soon as the SQL does mention it.
        install(&con, &["SELECT JSONB_AGG(x) FROM t"]).unwrap();
        assert!(con.execute_batch("SELECT jsonb_agg(1)").is_ok());
    }

    /// Only the named macro appears, so a pair using one shim does not silently acquire the rest.
    #[test]
    fn installation_is_selective() {
        let con = open_db().unwrap();
        install(&con, &["SELECT initcap(name) FROM t"]).unwrap();
        assert!(con.execute_batch("SELECT initcap('a')").is_ok());
        assert!(
            con.execute_batch("SELECT jsonb_array_length('[]')").is_err(),
            "a pair naming only initcap must not get the json shims"
        );
    }
}
