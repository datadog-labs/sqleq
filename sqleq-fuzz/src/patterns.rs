// Unless explicitly stated otherwise all files in this repository are licensed under the
// Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/).
// Copyright 2026-Present Datadog, Inc.

//! Text-level rewriting and analysis of the query pair.
//!
//! Several soundness rules live here:
//!   * nondeterministic *functions* (random/uuid/nextval/clock_timestamp) make a pair untestable → skip;
//!   * runtime *time* sources (now/current_*) must be frozen, else A and B — run microseconds apart —
//!     disagree spuriously;
//!   * row-locking clauses (`FOR UPDATE`/`SHARE`) don't affect the result bag → strip so DuckDB runs;
//!   * `LIMIT`/`OFFSET` over an unordered set makes the row *selection* nondeterministic. We bind
//!     pure limit/offset params large (never truncate); a remaining literal or a dual-purpose param
//!     limit marks the pair `nondet`, after which only cardinality differences are trusted (see pair.rs);
//!   * a pair whose two queries number their parameters differently is not testable by substituting
//!     one value per `$N` → [`misalignment`], and no verdict either way (see pair.rs).

use std::collections::{BTreeSet, HashMap, HashSet};
use std::sync::LazyLock;

use regex::Regex;

use sqlparser::ast::{SetExpr, Statement};
use sqlparser::dialect::PostgreSqlDialect;
use sqlparser::parser::Parser;

use crate::gen::{lit, Val};

macro_rules! re {
    ($name:ident, $pat:expr) => {
        static $name: LazyLock<Regex> = LazyLock::new(|| Regex::new($pat).unwrap());
    };
}

re!(RETURNING_WORD, r"(?i)\breturning\b");

// `EXPLAIN <stmt>` returns the engine's query plan, not the statement's rows. One side explained and
// the other not is a true difference about something nobody asked; both sides explained compares two
// DuckDB plan dumps for two Postgres queries, which differ for reasons that have nothing to do with
// equivalence and would be reported as a counterexample. Neither is a statement about the rewrite, so
// the pair is withdrawn rather than answered.
re!(EXPLAIN, r"(?i)^\s*explain\b");
// Truly nondeterministic functions: no way to make A and B agree → the pair is untestable.
re!(
    NONDET,
    r"(?i)\b(random|gen_random_uuid|uuid_generate\w*|uuid|nextval|clock_timestamp)\s*\("
);
// Time sources evaluated at runtime. now()/etc. take parens; current_timestamp/localtimestamp are
// bare keywords (a `()`-anchored pattern silently misses them).
re!(
    NOWFN,
    r"(?i)\b(?:now|statement_timestamp|transaction_timestamp)\s*\(\s*\)"
);
re!(
    NOWKW,
    r"(?i)\b(?:current_timestamp|localtimestamp)\b(?:\s*\(\s*\d*\s*\))?"
);
re!(
    CURTIMEKW,
    r"(?i)\b(?:current_time|localtime)\b(?:\s*\(\s*\d*\s*\))?"
);
re!(CURDATE, r"(?i)\bcurrent_date\b(?:\s*\(\s*\))?");
// Row-locking. It is *not* always at the end of the statement: most real uses sit inside a CTE or
// subquery (`... FOR UPDATE SKIP LOCKED) UPDATE ...`), so the terminator is captured and restored
// rather than anchored (the `regex` crate has no lookahead).
re!(
    LOCKING,
    r#"(?i)\bfor\s+(?:update|share|no\s+key\s+update|key\s+share)\b(?:\s+of\s+[\w\s,."]+?)?(?:\s+(?:nowait|skip\s+locked))?\s*(\)|;|$)"#
);
re!(
    LIMIT_PARAM,
    r"(?i)\b(?:limit|offset)\s+\$(\d+)|\bfetch\s+(?:first|next)\s+\$(\d+)"
);
re!(
    LIMIT_LIT,
    r"(?i)\b(?:limit|offset)\s+\d+|\bfetch\s+(?:first|next)\s+\d+"
);
re!(
    NONDET_AGG,
    r"(?i)\b(?:string_agg|group_concat|listagg|array_to_string|json_object_agg|jsonb_object_agg|json_agg|jsonb_agg)\s*\("
);
// Any statement that writes to the catalog or the data, however it starts.
re!(
    MUTATES,
    r"(?i)\b(?:create|insert|update|delete|drop|alter|merge|truncate|attach)\b"
);
re!(PARAM, r"\$(\d+)");
// Column compared against a param (both operand orders), and IN-lists.
re!(
    CMP_COL_PARAM,
    r"(?i)([A-Za-z_][\w.]*)\s*(?:>=|<=|<>|!=|=|>|<|ilike|like)\s*\$(\d+)"
);
re!(
    CMP_PARAM_COL,
    r"(?i)\$(\d+)\s*(?:>=|<=|<>|!=|=|>|<|ilike|like)\s*([A-Za-z_][\w.]*)"
);
/// A literal or placeholder, as it appears in an IN-list.
const VALUE: &str = r"(?:\$\d+|'(?:[^']|'')*'|-?\d+(?:\.\d+)?|null|true|false)";
/// A SQL type name as written in a cast: optionally quoted, optionally two words
/// (`character varying`, `double precision`), with an optional precision, time-zone suffix and `[]`.
const TY: &str = r#""?[A-Za-z_]\w*"?(?:\s+[A-Za-z_]\w*)?(?:\s*\(\s*\d+(?:\s*,\s*\d+)?\s*\))?(?:\s+(?:with|without)\s+time\s+zone)?(?:\s*\[\s*\])?"#;

// `col IN ($1, $2)` — a *value list* only, never `col IN (SELECT ...)`, whose params belong to the
// subquery's own predicates rather than to `col`. Spelling out the permitted contents (instead of
// matching any `(...)` and rejecting subqueries afterwards) is what makes nesting work: a failed
// match consumes nothing, so in `outer IN (SELECT ... WHERE inner IN ($2))` the inner list is still
// reached. Matching broadly and skipping would swallow it, leaving `$2` untyped.
static IN_LIST: LazyLock<Regex> = LazyLock::new(|| {
    let v = format!(r"{VALUE}(?:\s*::\s*{TY})?");
    Regex::new(&format!(
        r#"(?i)([A-Za-z_][\w."]*)\s+(?:not\s+)?in\s*\(\s*((?:{v}\s*,\s*)*{v})\s*\)"#
    ))
    .unwrap()
});
// `col <op> ANY($N)` / `ALL($N)`: the param *is* the whole array, so it links to the column exactly
// as a scalar comparison would. `CMP_COL_PARAM` cannot see through the `ANY(` and leaves `$N` untyped.
static ANY_COL_PARAM: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(&format!(
        r#"(?i)([A-Za-z_][\w."]*)\s*(?:>=|<=|<>|!=|=|>|<|ilike|like)\s*(?:any|all)\s*\(\s*\$(\d+)\s*(?:::\s*{TY})?\s*\)"#
    ))
    .unwrap()
});
// `$N = ANY(col)` / `$N <> ALL(col)`: the mirror of `ANY_COL_PARAM`. Postgres's ANY argument is an
// array and `$N` is one *element* of it, so `$N` stays a scalar and links to the column -- whose
// value pool is that column's elements now that an array column is materialized as a LIST, which is
// what makes the comparison hit rows instead of being vacuous. `CMP_PARAM_COL` reads the bare `ANY`
// as the column name, finds no such column, and links nothing.
static PARAM_ANY_COL: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(&format!(
        r#"(?i)\$(\d+)\s*(?:::\s*{TY})?\s*(?:>=|<=|<>|!=|=|>|<|ilike|like)\s*(?:any|all)\s*\(\s*([A-Za-z_][\w."]*)\s*\)"#
    ))
    .unwrap()
});
// Positions that require an *array*: the sole argument of ANY/ALL, or of `unnest`. Rewriting
// `col = ANY($1)` into `unnest($1)` is one of the optimizations this crate exists to check, so the same
// placeholder routinely appears in both forms and both must count as array uses.
// `ANY(ARRAY[$1, $2])` deliberately does not match — those params are scalars.
static ARRAY_ARG_PARAM: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(&format!(
        r"(?i)\b(?:any|all|unnest)\s*\(\s*\$(\d+)\s*(?:::\s*{TY})?\s*\)"
    ))
    .unwrap()
});
// Postgres array operators: overlap and the two containment directions.
//
// Both operands of the *array* forms are arrays, but the same three spellings are also jsonb
// containment (`@>`/`<@`) and range overlap/containment, where an operand is a jsonb value, a range,
// or even a bare scalar (`a_range @> $3::timestamp`). So the operator alone does not fix the type,
// and the rules below additionally require the *other* operand to be provably an array: a column the
// schema declares as one, a literal `ARRAY[...]`, or an explicit `::ty[]` cast. In practice that
// admits most rows with a `$N` as a whole operand and declines the rest — and every decline seen so
// far is a genuine non-array use: a range against a range, a range against a timestamp, a `jsonb`
// column against a `jsonb` value, an extension type carrying a containment operator of its own, and
// a column that resolves nowhere. Guessing "array" on those would bind a list into a scalar slot and
// cost the pair its run.
//
// One application with both operands captured as raw text, plus each operand's cast: enough to ask
// of either side whether it is a param, a column, an `ARRAY[...]`, or something else entirely.
// Note the `array\s*\[...]` alternative comes first — this crate's alternations are leftmost-first,
// and an `ARRAY[` matched as a bare identifier would lose the constructor.
static ARRAY_OP_APP: LazyLock<Regex> = LazyLock::new(|| {
    let operand = r#"(?:array\s*\[[^\[\]]*\]|\$\d+|[A-Za-z_][\w."]*)"#;
    Regex::new(&format!(
        r#"(?i)({operand})(?:\s*::\s*({TY}))?\s*(?:&&|@>|<@)\s*({operand})(?:\s*::\s*({TY}))?"#
    ))
    .unwrap()
});
// An explicit cast on a param (`$1::interval`, `$2::timestamp(6) without time zone`) states the type
// that position requires — the only direct type evidence for a param no column comparison reaches.
static PARAM_CAST: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(&format!(r"(?i)\$(\d+)\s*::\s*({TY})")).unwrap());

/// Either side an `EXPLAIN` → the pair compares plans, not results, and gets no verdict.
pub fn has_explain(a: &str, b: &str) -> bool {
    EXPLAIN.is_match(a) || EXPLAIN.is_match(b)
}

/// A pair using a truly nondeterministic function can't be tested for equivalence → NONDET-SKIP.
pub fn has_hard_nondet(a: &str, b: &str) -> bool {
    NONDET.is_match(a) || NONDET.is_match(b)
}

/// Freeze runtime time sources to constants and strip row-locking clauses.
pub fn freeze_time(sql: &str) -> String {
    let s = NOWFN.replace_all(sql, "TIMESTAMP '2020-06-01 00:00:00'");
    let s = NOWKW.replace_all(&s, "TIMESTAMP '2020-06-01 00:00:00'");
    let s = CURTIMEKW.replace_all(&s, "TIME '12:00:00'");
    let s = CURDATE.replace_all(&s, "DATE '2020-06-01'");
    // Drop the locking clause but keep whatever terminated it (`)` / `;` / end of string).
    let trimmed = s.trim_end();
    LOCKING.replace_all(trimmed, "${1}").into_owned()
}

/// The last dotted component of `name`, lower-cased, if it is a known column.
fn known_col(name: &str, cols: &HashSet<String>) -> Option<String> {
    // Quoted identifiers (`w0."CollectionId"`) are everywhere in ORM-generated SQL, and the quotes are
    // not part of the name — the schema side already stores it unquoted and lower-cased.
    let n = name
        .rsplit('.')
        .next()
        .unwrap_or(name)
        .trim_matches('"')
        .to_lowercase();
    if cols.contains(&n) {
        Some(n)
    } else {
        None
    }
}

/// Whether an operand is an `ARRAY[...]` constructor. Compared on bytes rather than by slicing at
/// index 5, which would panic on a multi-byte character inside a quoted identifier.
fn starts_with_array(operand: &str) -> bool {
    let b = operand.as_bytes();
    b.len() >= 5 && b[..5].eq_ignore_ascii_case(b"array")
}

/// `$N`, if `operand` is exactly a placeholder.
fn operand_param(operand: &str) -> Option<u32> {
    operand
        .trim()
        .strip_prefix('$')
        .and_then(|d| d.parse().ok())
}

/// Whether an operand of an array operator is provably an array: its own `::ty[]` cast, a literal
/// `ARRAY[...]`, or a column the schema declares as an array. A placeholder is not — that is the
/// question being asked — unless its own cast answers it.
fn operand_is_array(operand: &str, cast: Option<&str>, arraycols: &HashSet<String>) -> bool {
    if cast.is_some_and(|c| c.trim_end().ends_with(']')) {
        return true;
    }
    let t = operand.trim();
    if starts_with_array(t) {
        return true;
    }
    operand_param(t).is_none() && known_col(t, arraycols).is_some()
}

/// Map each `$N` compared against a known column to that column (first match wins), so its value can
/// be drawn from that column's generated data and equality filters actually match rows.
pub fn param_cols(a: &str, b: &str, cols: &HashSet<String>) -> HashMap<u32, String> {
    let mut out: HashMap<u32, String> = HashMap::new();
    for sql in [a, b] {
        for caps in IN_LIST.captures_iter(sql) {
            if let Some(col) = known_col(&caps[1], cols) {
                for m in PARAM.captures_iter(&caps[2]) {
                    out.entry(m[1].parse().unwrap())
                        .or_insert_with(|| col.clone());
                }
            }
        }
        for caps in ANY_COL_PARAM.captures_iter(sql) {
            if let Some(col) = known_col(&caps[1], cols) {
                out.entry(caps[2].parse().unwrap()).or_insert(col);
            }
        }
        for caps in PARAM_ANY_COL.captures_iter(sql) {
            if let Some(col) = known_col(&caps[2], cols) {
                out.entry(caps[1].parse().unwrap()).or_insert(col);
            }
        }
        for caps in CMP_COL_PARAM.captures_iter(sql) {
            if let Some(col) = known_col(&caps[1], cols) {
                out.entry(caps[2].parse().unwrap()).or_insert(col);
            }
        }
        for caps in CMP_PARAM_COL.captures_iter(sql) {
            if let Some(col) = known_col(&caps[2], cols) {
                out.entry(caps[1].parse().unwrap()).or_insert(col);
            }
        }
        // `col && $1`, `$1 <@ col`, `ARRAY[$1, $2]::text[] <@ col`, `col @> ARRAY[$3]`. The last two
        // shapes are why this reads the operands rather than reusing `CMP_*`: there the params are the
        // array's *elements*, they stay scalars, and the only thing that makes them discriminating
        // instead of merely runnable is drawing them from the elements the column actually holds.
        for caps in ARRAY_OP_APP.captures_iter(sql) {
            let (l, r) = (caps[1].trim(), caps[3].trim());
            for (operand, other) in [(l, r), (r, l)] {
                let Some(col) = known_col(other, cols) else {
                    continue;
                };
                if let Some(n) = operand_param(operand) {
                    out.entry(n).or_insert(col);
                } else if starts_with_array(operand) {
                    for m in PARAM.captures_iter(operand) {
                        out.entry(m[1].parse().unwrap())
                            .or_insert_with(|| col.clone());
                    }
                }
            }
        }
    }
    out
}

/// Params bound to an *array* rather than a scalar: the sole argument of `ANY(...)`/`ALL(...)`/
/// `unnest(...)`, or a whole operand of an array operator (`col && $1`, `$1 <@ tags`) where the other
/// operand is provably an array. Postgres `col = ANY($1)` takes an array; binding a scalar leaves
/// DuckDB unnesting a non-array ("UNNEST not supported here") and the pair never runs at all.
///
/// `arraycols` is the set of columns the schema declares as arrays — the evidence that separates
/// `tags && $1` from `valid_period && $2::daterange`. See [`ARRAY_OP_APP`] for why the operator
/// alone is not enough.
///
/// A param is only reported when *every* one of its occurrences is an array position: some corpus
/// pairs use one placeholder as both an array and a scalar, and binding a list there would merely
/// trade one bind error for another.
// Documented in terms of the private pattern table it consults.
#[allow(rustdoc::private_intra_doc_links)]
pub fn array_params(a: &str, b: &str, arraycols: &HashSet<String>) -> BTreeSet<u32> {
    // Occurrences are identified by (side, byte offset) rather than counted, because two rules can
    // land on the same one — `$1 && $2` is a single application and one array position for each param
    // — and a double count would push `in_array` past `total` and silently drop a param that is an
    // array everywhere it appears.
    let mut in_array: HashMap<u32, HashSet<(usize, usize)>> = HashMap::new();
    let mut total: HashMap<u32, HashSet<(usize, usize)>> = HashMap::new();
    for (side, sql) in [a, b].into_iter().enumerate() {
        for caps in ARRAY_ARG_PARAM.captures_iter(sql) {
            let m = caps.get(1).unwrap();
            in_array
                .entry(m.as_str().parse().unwrap())
                .or_default()
                .insert((side, m.start()));
        }
        for caps in ARRAY_OP_APP.captures_iter(sql) {
            let (l, r) = (caps.get(1).unwrap(), caps.get(3).unwrap());
            let lc = caps.get(2).map(|m| m.as_str());
            let rc = caps.get(4).map(|m| m.as_str());
            let (l_arr, r_arr) = (
                operand_is_array(l.as_str(), lc, arraycols),
                operand_is_array(r.as_str(), rc, arraycols),
            );
            for (m, own, other) in [(l, l_arr, r_arr), (r, r_arr, l_arr)] {
                // `own` is the param's own `::ty[]` cast saying so; `other` is the operand it is
                // compared against. A param inside `ARRAY[...]` is an element, not an array, and is
                // deliberately not reached here.
                // `+ 1` puts the offset on the digits, matching what `ARRAY_ARG_PARAM` and `PARAM`
                // report — those capture `(\d+)`, this captures the whole `$N` operand.
                if let (Some(n), true) = (operand_param(m.as_str()), own || other) {
                    in_array.entry(n).or_default().insert((side, m.start() + 1));
                }
            }
        }
        for caps in PARAM.captures_iter(sql) {
            let m = caps.get(1).unwrap();
            total
                .entry(m.as_str().parse().unwrap())
                .or_default()
                .insert((side, m.start()));
        }
    }
    in_array
        .iter()
        .filter(|(n, seen)| total.get(n).is_some_and(|t| t.len() == seen.len()))
        .map(|(n, _)| *n)
        .collect()
}

/// Map each `$N` carrying an explicit `::type` cast to that raw type name (first match wins).
pub fn param_casts(a: &str, b: &str) -> HashMap<u32, String> {
    let mut out: HashMap<u32, String> = HashMap::new();
    for sql in [a, b] {
        for caps in PARAM_CAST.captures_iter(sql) {
            out.entry(caps[1].parse().unwrap())
                .or_insert_with(|| caps[2].trim().to_string());
        }
    }
    out
}

/// The `$N` placeholders one query mentions.
///
/// Text-level, exactly like [`substitute`], and deliberately so: what this returns is the set
/// substitution will replace, so a check built on it is a check on the binding sqleq-fuzz actually
/// performs rather than on one a parser would infer for it.
pub fn params_of(sql: &str) -> BTreeSet<u32> {
    PARAM
        .captures_iter(sql)
        .map(|caps| caps[1].parse().unwrap())
        .collect()
}

/// All `$N` placeholders appearing in the pair.
pub fn param_nums(a: &str, b: &str) -> BTreeSet<u32> {
    let mut out = params_of(a);
    out.extend(params_of(b));
    out
}

/// `$1, $2, $3` — bounded, because a query can mention more parameters than a message should carry.
fn list(ns: impl IntoIterator<Item = u32>) -> String {
    let ns: Vec<u32> = ns.into_iter().collect();
    if ns.is_empty() {
        return "none".to_string();
    }
    let mut s = ns
        .iter()
        .take(8)
        .map(|n| format!("${n}"))
        .collect::<Vec<_>>()
        .join(", ");
    if ns.len() > 8 {
        s.push_str(&format!(", … ({} more)", ns.len() - 8));
    }
    s
}

/// Evidence that binding one value per `$N` across the pair asks a question the caller did not.
///
/// Substitution puts the same value in query A's `$N` and query B's `$N` ([`substitute`], driven by
/// the one `binds` map `pair::test_pair` builds). That is **index binding**: `$1` on the left is `$1`
/// on the right because they share a number. What the caller means is **intended binding** — `$1` on
/// the left is whichever placeholder on the right the application fills from the same value — and a
/// rewrite that drops, adds or reorders a placeholder renumbers everything after it. Nothing in a
/// `(A, B, DDL)` row records the call site, so the two can only be told apart by evidence, and this is
/// the exact half of that evidence: **the two queries mention different sets of `$N`, and both are
/// parameterized at all.**
///
/// ## Why a *disprover* has to be stricter here than the prover is
///
/// The frontend runs the same test (`params::check_arity`) and deliberately exempts sets that differ
/// *disjointly* — no index on both sides. Its argument is sound and does not carry over. For a
/// **prover**, binding the two queries' parameters independently quantifies over a *superset* of the
/// caller's diagonal, so a proof of the wider statement implies the narrower one: incomplete at worst,
/// never unsound. For a **disprover** the same move is fatal in the other direction — a witness found
/// anywhere in the wider space need not lie on the caller's diagonal. `WHERE a = $1` against
/// `WHERE a = $2` shares no index, and drawing 1 for `$1` and 2 for `$2` "disproves" a pair the caller
/// filled from one value and which is equivalent. So the condition here is *any* index in one query and
/// not the other, which is strictly wider than `check_arity`'s, and the two are not meant to agree.
///
/// Real pairs do not force this: every differing parameter set observed so far **overlaps**, and the
/// disjoint class has been empty, so the wider rule costs nothing measurable here. It is the duality
/// above that argues for it, not a measurement.
///
/// ## What it does not catch
///
/// A pure permutation — the same set on both sides with two indices swapped — is invisible to an
/// *arity* test like this one, and index binding then checks the wrong diagonal with nothing to flag.
/// That is the residual hole, the same one `params::check_roles` exists to narrow on the prover side
/// (where it has yet to find a detectable case).
///
/// It is not invisible to a *role* test, and real pairs say so. One observed pair has
/// `first_name = $4 AND last_name = $5` against `last_name = $4 AND first_name = $5`: the two are
/// swapped. This function refuses that pair, but only incidentally — its sets differ because
/// `SELECT $1` became `SELECT 1` — and it had been reporting `NO-COUNTEREXAMPLE`, because two `text`
/// columns drawn from the biased value pool rarely separate under a swap. So the natural next step, the
/// analogue of `check_roles` built on [`param_cols`]' column evidence **per query** rather than per pair,
/// has a real target rather than a hypothetical one: some of the claims this function withdraws have a
/// shared index that provably compares against disjoint columns on the two sides, and for those the
/// withdrawal is not a cost but a correction.
///
/// ## A refinement that looks free and is not
///
/// If one side's indices have a **gap** (`{1,2,4,5}`), that side cannot be the product of a rewrite that
/// dropped a placeholder and renumbered the rest, since renumbering yields a contiguous set — so the
/// shared indices ought to still mean what they meant, and the pair ought to be exempt. A real share of
/// misaligned pairs would qualify. **The swapped pair above refutes it**: `B`'s set is `{2,…,6}`, a gap
/// at `$1`, and its `$4`/`$5` are swapped all the same. A leading gap comes from folding the *first*
/// parameter away, which says nothing about the order of the rest. Adopting it would readmit a claim we
/// can show is about the wrong pair, so the pairs it would recover stay refused. (The other tempting
/// refinement — "the orphans all sit above every shared index" — is refuted by a real pair too, as
/// `params`' module docs record.)
///
/// Returns the human-readable evidence, or `None` when index binding is the caller's binding as far as
/// the text can tell: equal sets, or one query with no parameters at all (nothing is being identified
/// across the pair, so substituting values for the other query's parameters is exactly the caller's
/// own quantification).
pub fn misalignment(a: &str, b: &str) -> Option<String> {
    let (pa, pb) = (params_of(a), params_of(b));
    if pa == pb || pa.is_empty() || pb.is_empty() {
        return None;
    }
    let shared = pa.intersection(&pb).copied();
    let orphans = pa.symmetric_difference(&pb).copied();
    Some(format!(
        "arity: query A uses {} parameter(s), query B uses {}, sharing {}; in one query only {}",
        pa.len(),
        pb.len(),
        list(shared),
        list(orphans)
    ))
}

/// Params used directly as a `LIMIT`/`OFFSET`/`FETCH` count.
pub fn limit_params(a: &str, b: &str) -> BTreeSet<u32> {
    let mut out = BTreeSet::new();
    for sql in [a, b] {
        for caps in LIMIT_PARAM.captures_iter(sql) {
            // Exactly one of the two alternation groups is present per match.
            for g in [caps.get(1), caps.get(2)].into_iter().flatten() {
                out.insert(g.as_str().parse().unwrap());
            }
        }
    }
    out
}

pub fn has_literal_limit(a: &str, b: &str) -> bool {
    LIMIT_LIT.is_match(a) || LIMIT_LIT.is_match(b)
}

pub fn has_nondet_agg(a: &str, b: &str) -> bool {
    NONDET_AGG.is_match(a) || NONDET_AGG.is_match(b)
}

/// Whether running this statement can leave objects behind in the catalog. The trials of a pair share
/// one database, so anything created has to be swept before the next side runs — and `is_query` alone
/// does not settle it, since a `WITH ... INSERT` reads as a query by its leading keyword.
pub fn may_create_objects(stmt: &str) -> bool {
    MUTATES.is_match(stmt)
}

/// Whether this statement carries a `RETURNING` clause.
///
/// A mutation with `RETURNING` has **two** observable results -- the new table state and the
/// returned bag -- and the two are independent: `UPDATE t SET c = c WHERE true RETURNING c` and the
/// same statement `WHERE false` leave the table byte-identical and return different bags. Observing
/// only the table therefore hands out a `no-counterexample` that never looked at half the meaning,
/// so [`crate::duck::run_side`] observes the bag as well and [`crate::pair::test_pair`] refuses a
/// pair that carries the clause on one side only.
///
/// Read from the AST, never the text: `RETURNING` is not a reserved word, so a column or alias may
/// be spelled `returning` and a string literal may contain it. Parse failure answers `false`, which
/// is the pre-existing behaviour (the bag went unobserved) rather than a new refusal.
pub fn has_returning(stmt: &str) -> bool {
    // Cheap gate first: the parse is the expensive part and almost no statement reaches it.
    if !RETURNING_WORD.is_match(stmt) {
        return false;
    }
    match Parser::parse_sql(&PostgreSqlDialect {}, stmt) {
        Ok(stmts) => stmts.iter().any(stmt_returns),
        Err(_) => false,
    }
}

/// `RETURNING` on a statement, looking through the `WITH`-wrapped forms: CTEs attach to a query, so
/// `WITH x AS (..) DELETE ..` parses as a [`Statement::Query`] whose body is a [`SetExpr::Delete`].
fn stmt_returns(stmt: &Statement) -> bool {
    match stmt {
        Statement::Insert(i) => i.returning.is_some(),
        Statement::Update(u) => u.returning.is_some(),
        Statement::Delete(d) => d.returning.is_some(),
        Statement::Query(q) => match &*q.body {
            SetExpr::Insert(inner) | SetExpr::Update(inner) | SetExpr::Delete(inner) => {
                stmt_returns(inner)
            }
            _ => false,
        },
        _ => false,
    }
}

/// Substitute every `$N` with the SQL literal of its bound value.
pub fn substitute(sql: &str, binds: &HashMap<u32, Val>) -> String {
    PARAM
        .replace_all(sql, |caps: &regex::Captures| {
            let n: u32 = caps[1].parse().unwrap();
            binds.get(&n).map(lit).unwrap_or_else(|| "NULL".to_string())
        })
        .into_owned()
}
