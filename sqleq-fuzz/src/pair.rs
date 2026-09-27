//! The per-pair test loop: generate valid instances, run both sides, look for a counterexample.

use std::collections::HashMap;

use rand::rngs::StdRng;
use rand::seq::SliceRandom;
use rand::{Rng, SeedableRng};

use sqlparser::ast::{SetExpr, Statement};
use sqlparser::dialect::PostgreSqlDialect;
use sqlparser::parser::Parser;

use crate::duck::{accepted_rows, open_db, run_side, table_forms, RowData};
use crate::gen::{
    array_element_type, cast_target, lit, randval, randval_cast, randval_col, randval_need,
    CastTarget, Val,
};
use crate::patterns as pat;
use crate::rewrite;
use crate::schema::{parse_schema, VType};
use crate::shim;
use crate::typing;

/// Test configuration.
#[derive(Clone, Copy, Debug)]
pub struct Config {
    pub trials: usize,
    pub nrows: usize,
    pub seed: u64,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            trials: 120,
            nrows: 5,
            seed: 0,
        }
    }
}

/// The outcome of testing one pair.
#[derive(Clone, Debug)]
pub enum Verdict {
    /// A truly nondeterministic function is present — untestable.
    NondetSkip,
    /// The two sides have no shared observable, so running them would compare something neither
    /// pair member is about (carries which shape it was). Two reasons reach this: a side is an
    /// `EXPLAIN`, which has query plans rather than query results (see
    /// [`crate::patterns::has_explain`]), or one side is a query and the other a mutation.
    NotComparable(String),
    /// No parseable table schema.
    NoSchema,
    /// No known table is referenced by the queries.
    NoTables,
    /// The two queries number their parameters differently, so binding one value per `$N` across the
    /// pair tests a statement the caller never made — no verdict either way (carries the evidence).
    /// See [`crate::patterns::misalignment`] for why this is a withdrawal and not a counterexample.
    ParamMisaligned(String),
    /// A counterexample instance was found (carries a human-readable description).
    NotEquivalent(String),
    /// No counterexample found within the trial budget.
    NoCounterexample,
    /// No counterexample found, but only some trials ran: `ok` of them really compared both sides.
    /// Labelled `NO-COUNTEREXAMPLE` (that *is* the finding); the count is reported alongside so thin
    /// coverage stays visible instead of masquerading as a full run.
    NoCounterexamplePartial { ok: usize, last_err: String },
    /// *No* trial ever ran both sides — genuinely untestable (carries the last error message).
    Error(String),
}

impl Verdict {
    /// The corpus-compatible label (matches the Python tester's verdict strings).
    pub fn label(&self) -> String {
        match self {
            Verdict::NondetSkip => "NONDET-SKIP".to_string(),
            Verdict::NotComparable(d) => format!("NOT-COMPARABLE:{d}"),
            Verdict::NoSchema => "NO-SCHEMA".to_string(),
            Verdict::NoTables => "NO-TABLES".to_string(),
            // Label plus evidence, like `ERROR:` — a consumer that buckets on the part before the
            // first `:` sees `PARAM-MISALIGNED` and one that reads the whole string sees why.
            Verdict::ParamMisaligned(d) => format!("PARAM-MISALIGNED:{d}"),
            Verdict::NotEquivalent(_) => "NOT-EQUIVALENT".to_string(),
            Verdict::NoCounterexample | Verdict::NoCounterexamplePartial { .. } => {
                "NO-COUNTEREXAMPLE".to_string()
            }
            Verdict::Error(e) => format!("ERROR:{e}"),
        }
    }

    /// For a partially-run pair, how many trials compared both sides and what the last error was.
    pub fn partial(&self) -> Option<(usize, &str)> {
        match self {
            Verdict::NoCounterexamplePartial { ok, last_err } => Some((*ok, last_err.as_str())),
            _ => None,
        }
    }
}

/// Whether a side is a *query* — something with rows to read — as opposed to a statement whose
/// effect is on the tables.
///
/// The distinction decides how [`run_side`] observes the side: a query is observed by the rows it
/// returns, a statement by the contents of every table afterwards. Getting it wrong on one side of
/// a pair compares the two sides on *different observables*, and then `NO-COUNTEREXAMPLE` is not
/// reachable at all — every such pair reports as refuted, whatever it is.
///
/// Which is why the leading keyword cannot decide it. sqlparser parses `WITH t AS (..) DELETE ..`
/// as a [`Statement::Query`] whose body is a [`SetExpr::Delete`] — the CTEs attach to a query
/// wrapper, but the statement is still a `DELETE` and still has no rows to read. A leading `WITH`
/// therefore has to be parsed before it can be believed; the other prefixes are unambiguous, so
/// they keep the cheap path and an unparseable `WITH` falls back to the old reading.
fn is_query(stmt: &str) -> bool {
    let t = stmt.trim_start();
    if t.starts_with('(') {
        return true; // `(SELECT ...) UNION ...`
    }
    let verb = t.split_whitespace().next().unwrap_or("").to_uppercase();
    if verb == "SELECT" {
        return true;
    }
    if verb != "WITH" {
        return false;
    }
    match Parser::parse_sql(&PostgreSqlDialect {}, stmt).as_deref() {
        Ok([Statement::Query(q)]) => !matches!(
            &*q.body,
            SetExpr::Delete(_) | SetExpr::Insert(_) | SetExpr::Update(_)
        ),
        _ => true,
    }
}

/// The first line of a DuckDB error, bounded so a verdict stays one grep-able line.
///
/// The width is not arbitrary. What identifies the *cause* of a binder error is the argument-type
/// list — `'~~(VARCHAR, INTEGER_LITERAL)'` says a parameter was typed wrong, `'~~(VARCHAR, VARCHAR)'`
/// would say something else entirely — and in DuckDB's phrasing that list starts past column 90. Cut
/// there and every such row reads as the same undifferentiated failure.
fn err_msg(e: &duckdb::Error) -> String {
    e.to_string()
        .lines()
        .next()
        .unwrap_or("")
        .chars()
        .take(240)
        .collect()
}

/// Render a found counterexample (bound params + the instance rows) for reporting.
fn describe(binds: &HashMap<u32, Val>, rowdata: &RowData) -> String {
    let mut ps: Vec<(u32, &Val)> = binds.iter().map(|(k, v)| (*k, v)).collect();
    ps.sort_by_key(|(k, _)| *k);
    let params = ps
        .iter()
        .map(|(k, v)| format!("${k}={}", lit(v)))
        .collect::<Vec<_>>()
        .join(", ");
    let tables = rowdata
        .iter()
        .map(|(t, rows)| {
            let rs = rows
                .iter()
                .map(|r| format!("({})", r.iter().map(lit).collect::<Vec<_>>().join(",")))
                .collect::<Vec<_>>()
                .join("; ");
            format!("{t}=[{rs}]")
        })
        .collect::<Vec<_>>()
        .join("  ");
    if params.is_empty() {
        tables
    } else {
        format!("params: {params}  |  {tables}")
    }
}

/// Test one query pair against its DDL. Any difference on a valid, deterministic instance is a sound
/// counterexample → NOT-EQUIVALENT.
///
/// Except on a pair whose two queries number their parameters differently, where substituting one value
/// per `$N` compares two queries the caller never paired: **a misaligned pair's *claim* is withdrawn,
/// and its untestability is reported as it is.** Both halves matter. `NOT-EQUIVALENT` and
/// `NO-COUNTEREXAMPLE` are the only things this function says about the *pair*, and index binding is
/// what taints them, so they become `PARAM-MISALIGNED`. Everything else it emits — `NONDET-SKIP`,
/// `NO-SCHEMA`, `NO-TABLES`, `NOT-COMPARABLE`, `ERROR` — is a statement about sqleq-fuzz's own reach, is true whatever the
/// numbering is, and is what a reader triaging that reach needs to see; so those win, which also keeps
/// the `PARAM-MISALIGNED` bucket meaning *"the numbering is the only thing in the way"*.
///
/// The frontend's rule at the same fork is two-directional — a misalignment outranks any refusal it
/// could have manufactured and yields to any it could not, decided by a counterfactual re-run
/// (`params::root_cause_lowered`). It has to be: there the competing refusals are the frontend's own
/// unlowerable constructs, so which one is reported is a real question. Here they are not comparable,
/// so the fork does not need deciding and no counterfactual is built. The price is that an `ERROR` the
/// identification did itself provoke — a value drawn from an `integer` column landing in a `uuid`
/// comparison — still reports as that type error. Surveying real pairs that land here, about half carry
/// an error no binding could have produced (a name or a type DuckDB does not have); the rest are
/// parameter-typing errors that the identification *can* provoke but that this crate's own
/// `VType::Integer` fallback produces on well-aligned pairs too, so which of the two it was is not
/// separated here. It stays
/// recoverable either way: whether a pair is misaligned is a text-level property of the row, independent
/// of any verdict.
pub fn test_pair(a: &str, b: &str, ddl: &str, cfg: Config) -> Verdict {
    // First, because it is a fact about what kind of statement each side *is* rather than about
    // anything inside it: an explained statement has no rows to compare, so nothing downstream —
    // nondeterminism, schema, parameters — is a question worth asking about the pair.
    if pat::has_explain(a, b) {
        return Verdict::NotComparable("explain".to_string());
    }
    if pat::has_hard_nondet(a, b) {
        return Verdict::NondetSkip;
    }
    let schema = parse_schema(ddl);
    if schema.is_empty() {
        return Verdict::NoSchema;
    }
    // `unqualify_stars` runs on the pristine text, ahead of every text-level rewrite, so it never
    // depends on one of them leaving the statement parseable. `parenthesize_json_ops` runs on its
    // output rather than on the pristine text: both read their edits off a Postgres parse, and
    // `unqualify_stars` only ever shortens a qualifier, so the statement the second one parses is
    // still the one the caller wrote — and if it somehow is not, that pass returns its input
    // unchanged and we merely lose the parenthesization on this row. Neither can add or remove a
    // `$N`, so the misalignment check below still sees the parameter numbering the caller actually
    // wrote.
    //
    // One closure, applied to both sides, so the two sides cannot drift apart in how they are
    // prepared — which is the shape the `is_query` defect took.
    let prep = |sql: &str| {
        let unqualified = rewrite::unqualify_stars(sql);
        pat::freeze_time(&rewrite::parenthesize_json_ops(&unqualified))
    };
    let a = prep(a);
    let b = prep(b);
    let forms = table_forms(&a, &b, &schema);
    if forms.is_empty() {
        return Verdict::NoTables;
    }
    let finals: Vec<String> = forms.keys().cloned().collect();

    // Held, not returned: the pair still runs, because whether it runs *at all* is the one question a
    // misaligned pair has left and only the trials can answer it.
    let misaligned = pat::misalignment(&a, &b);

    // Column name -> membership, and column name -> (table, index, element type, is array) for
    // drawing param values. `vt` is the *element* domain when the column is an array, so every
    // consumer below has to say which of the two it means.
    let mut coltype: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut colloc: HashMap<String, (String, usize, VType, bool)> = HashMap::new();
    for t in &finals {
        for (j, c) in schema[t].cols.iter().enumerate() {
            coltype.insert(c.name.clone());
            colloc
                .entry(c.name.clone())
                .or_insert((t.clone(), j, c.vt, c.array));
        }
    }

    // Types the syntax fixes, for the slots no column evidence reaches. Read from both queries into
    // one map, like every other param analysis here, so the pair still binds one value per `$N`.
    //
    // Array columns are deliberately withheld. `param_needs` types a `$N` from the column across a
    // comparison, and an array column would type it as the scalar element domain -- re-introducing,
    // one layer up, the very confusion between `text` and `text[]` this change removes. Declining is
    // the correct answer: `array_params` and `param_casts` are what reach those slots.
    let coltypes: HashMap<String, VType> = colloc
        .iter()
        .filter(|(_, (_, _, _, array))| !array)
        .map(|(name, (_, _, vt, _))| (name.clone(), *vt))
        .collect();
    let pneed = typing::param_needs(&a, &b, &coltypes);

    let pcol = pat::param_cols(&a, &b, &coltype);
    let pcast = pat::param_casts(&a, &b);
    // The array columns are what let `array_params` tell `tags && $1` (an array param) from
    // `valid_period && $2::daterange` (a range, and a scalar slot) -- the operator spellings overlap.
    let arraycols: std::collections::HashSet<String> = colloc
        .iter()
        .filter(|(_, (_, _, _, arr))| *arr)
        .map(|(n, _)| n.clone())
        .collect();
    let parray = pat::array_params(&a, &b, &arraycols);
    let pnums = pat::param_nums(&a, &b);
    let limit_params = pat::limit_params(&a, &b);
    // Only neutralize a limit/offset param that is NOT also a value predicate (e.g. keyset pagination
    // `WHERE id > $2 ... OFFSET $2`): binding a dual-purpose param large would hide the difference.
    let neutralize: std::collections::HashSet<u32> = limit_params
        .iter()
        .copied()
        .filter(|n| !pcol.contains_key(n))
        .collect();
    // Nondeterministic row selection/content: a literal LIMIT/OFFSET, a kept (non-neutralized) param
    // limit that can still truncate, or a string-flattening aggregate.
    let kept_limit = limit_params.iter().any(|n| !neutralize.contains(n));
    let nondet = pat::has_literal_limit(&a, &b) || kept_limit || pat::has_nondet_agg(&a, &b);

    let is_query_a = is_query(&a);
    let is_query_b = is_query(&b);
    // A query on one side and a mutation on the other have no shared observable to compare. The
    // choice below is a disjunction, so a mixed pair is compared by *table state* -- an observable
    // only one of the two sides has. That makes the verdict uninformative whichever way it lands:
    // the query side never writes, so the states differ exactly when the mutation side is
    // non-vacuous (a refutation that only restates that a SELECT is not an UPDATE), and they agree
    // exactly when it is vacuous (a `no-counterexample` that tested nothing, the same empty
    // reassurance the `is_query` observable bug used to hand out). Neither belongs in the numbers,
    // so refuse the pair and say why.
    if is_query_a != is_query_b {
        return Verdict::NotComparable(format!(
            "mixed-kind: {} vs {}",
            if is_query_a { "query" } else { "mutation" },
            if is_query_b { "query" } else { "mutation" },
        ));
    }
    // `RETURNING` adds a second observable, so a pair carrying it on one side only is the
    // mixed-kind problem one level down: side A's meaning is (bag, table) and side B's is (table),
    // and comparing them compares an observable only one side has. The two are also genuinely
    // different statements -- Postgres hands the caller rows in one case and nothing in the other --
    // so neither verdict would be about the rewrite. Real DML rewrites do come in this shape;
    // refuse them and say so rather than compare the halves that happen to line up.
    let ret_a = pat::has_returning(&a);
    let ret_b = pat::has_returning(&b);
    if ret_a != ret_b {
        return Verdict::NotComparable(format!(
            "one-sided RETURNING: {} vs {}",
            if ret_a { "returning" } else { "none" },
            if ret_b { "returning" } else { "none" },
        ));
    }

    // If *either* side can leave objects behind, both sides get the thorough catalog sweep: the two
    // sides and every trial share one database, so B must be cleaned up after A just as much.
    let mutates =
        !is_query_a || !is_query_b || pat::may_create_objects(&a) || pat::may_create_objects(&b);

    // One database for the whole pair: building one per side per trial cost more than the queries.
    let con = match open_db() {
        Ok(c) => c,
        Err(e) => return Verdict::Error(err_msg(&e)),
    };
    // Postgres functions DuckDB has no name for, defined as macros before anything runs — only the
    // ones this pair actually mentions, and identically for both sides. Without them both sides
    // fail to bind and the pair is never tried at all.
    if let Err(e) = shim::install(&con, &[&a, &b]) {
        return Verdict::Error(err_msg(&e));
    }

    let mut rng = StdRng::seed_from_u64(cfg.seed);
    let mut last_err: Option<String> = None;
    // Trials in which *both* sides ran. A pair where some trials error and the rest agree was really
    // tested; reporting it as ERROR (which the sticky `last_err` alone would do) hides that.
    let mut ok_trials = 0usize;

    for _ in 0..cfg.trials {
        let rowdata: RowData = finals
            .iter()
            .map(|t| {
                let rows = (0..cfg.nrows)
                    .map(|_| {
                        schema[t]
                            .cols
                            .iter()
                            .map(|c| randval_col(c, &mut rng))
                            .collect::<Vec<_>>()
                    })
                    .collect::<Vec<_>>();
                (t.clone(), rows)
            })
            .collect();

        let mut binds: HashMap<u32, Val> = HashMap::new();
        for &n in &pnums {
            let loc = pcol.get(&n).and_then(|c| colloc.get(c));
            // The values the linked column actually holds, as *scalars*. An array column holds lists,
            // and one level of flattening is what makes both of its shapes work off one draw: a
            // scalar param (`ARRAY[$1]::text[] <@ tags`) needs a single element, and an array param
            // (`tags && $1`) needs elements to build a list from. Either way what has to come out of
            // `pick` is an element, so `present` is the element pool and `is_array` decides the
            // wrapping, exactly as it already does for a scalar column.
            let present: Vec<Val> = match loc {
                Some((t, idx, _, col_array)) => rowdata[t]
                    .iter()
                    .filter(|r| r[*idx] != Val::Null)
                    .flat_map(|r| match (&r[*idx], col_array) {
                        (Val::List(elems), true) => elems
                            .iter()
                            .filter(|e| **e != Val::Null)
                            .cloned()
                            .collect::<Vec<_>>(),
                        (v, _) => vec![v.clone()],
                    })
                    .collect(),
                None => Vec::new(),
            };
            // An explicit `::type` cast pins the type this position requires, so a value drawn from a
            // differently-typed column would not survive it. Otherwise nothing changes.
            //
            // A *text* cast is the exception: every value in the generation domain renders as valid
            // text, so it constrains nothing and must not displace direct column evidence — under
            // `WHERE int_col = $1::text` the value still has to be an integer.
            //
            // For an array param the cast names the *array* type (`uuid[]`); the elements are what
            // have to satisfy it.
            let is_array = parray.contains(&n);
            let cast = pcast
                .get(&n)
                .map(|t| {
                    if is_array {
                        array_element_type(t)
                    } else {
                        t.clone()
                    }
                })
                .and_then(|t| cast_target(&t))
                .filter(|ct| loc.is_none() || *ct != CastTarget::V(VType::Varchar));
            let col_survives_cast = match (cast, loc) {
                // `vt` is the element domain when the column is an array, and `cast` was reduced to
                // its element type just above when the param is one, so both sides of this
                // comparison are element types in every combination.
                (Some(CastTarget::V(v)), Some((_, _, vt, _))) => v == *vt,
                // interval/json/time demand a specific string shape that no column's generation
                // domain produces — 'c' is a VARCHAR but not an interval.
                (Some(_), _) => false,
                (None, _) => true,
            };
            // One scalar draw: a value the column actually holds when the cast permits it, else one
            // the cast will accept. Array elements reuse this so they stay equally well typed.
            let pick = |rng: &mut StdRng| -> Val {
                // A jointly-constrained slot is the one case where the need outranks the evidence
                // below rather than backing it up. Both sources here agree on a canonical integer --
                // `pcast` from the `$N::int4`, and `loc` from the `c = $N` the rewrite left on that
                // same side -- and agreeing is exactly the problem: the text comparison on the
                // *other* side then reduces to the identical test. This arm stays as narrow as that
                // argument: no other `Need` preempts anything, so every other slot keeps the
                // strictly-additive last-resort position it has today.
                if pneed.get(&n) == Some(&typing::Need::NumericString) {
                    return randval_need(typing::Need::NumericString, rng);
                }
                if col_survives_cast && !present.is_empty() && rng.gen_bool(0.75) {
                    present.choose(rng).unwrap().clone() // bias to a value that exists in the column
                } else if let Some(ct) = cast {
                    randval_cast(ct, rng)
                } else if let Some((_, _, vt, _)) = loc {
                    randval(*vt, false, rng)
                } else if let Some(need) = pneed.get(&n) {
                    // Strictly additive: this arm is only ever reached where the value would have been
                    // an integer drawn from nothing.
                    randval_need(*need, rng)
                } else {
                    randval(VType::Integer, false, rng)
                }
            };
            let v = if neutralize.contains(&n) {
                Val::Int(1_000_000_000) // pure row-limit param -> never truncates
            } else if is_array {
                // 1-3 elements: enough to match real rows often, few enough that the predicate stays
                // selective and can still discriminate the two sides.
                let k = rng.gen_range(1..=3);
                let mut elems = Vec::with_capacity(k);
                for _ in 0..k {
                    elems.push(pick(&mut rng));
                }
                Val::List(elems)
            } else {
                pick(&mut rng)
            };
            binds.insert(n, v);
        }

        let sub_a = pat::substitute(&a, &binds);
        let sub_b = pat::substitute(&b, &binds);

        let (ra, sa) = match run_side(
            &con, &sub_a, is_query_a, ret_a, mutates, &forms, &schema, &rowdata,
        ) {
            Ok(v) => v,
            Err(e) => {
                last_err = Some(err_msg(&e));
                continue;
            }
        };
        let (rb, sb) = match run_side(
            &con, &sub_b, is_query_b, ret_b, mutates, &forms, &schema, &rowdata,
        ) {
            Ok(v) => v,
            Err(e) => {
                last_err = Some(err_msg(&e));
                continue;
            }
        };

        ok_trials += 1;

        // No claim is available for a misaligned pair whichever way this trial comes out, so one trial
        // that ran both sides has settled everything still open — and comparing the remaining trials
        // under a binding we are about to disown would only cost time.
        if misaligned.is_some() {
            break;
        }

        if ra != rb {
            if nondet && sa == sb {
                continue; // equal cardinality + nondeterministic clause -> not a sound counterexample
            }
            // Report the rows the database accepted, not the rows we generated: a row violating
            // UNIQUE or NOT NULL never entered the table the trial ran against. Falling back to the
            // generated set on a replay error keeps a witness we can still read.
            let shown = accepted_rows(&con, &forms, &schema, &rowdata).unwrap_or(rowdata);
            return Verdict::NotEquivalent(describe(&binds, &shown));
        }
    }

    if let Some(detail) = misaligned {
        return match (ok_trials, last_err) {
            // Nothing ever ran: untestable for a reason the numbering did not create, and that reason
            // is the one a reader can act on.
            (0, Some(e)) => Verdict::Error(e),
            _ => Verdict::ParamMisaligned(detail),
        };
    }

    match (ok_trials, last_err) {
        (0, Some(e)) => Verdict::Error(e), // nothing ever ran -> genuinely untestable
        (_, Some(e)) => Verdict::NoCounterexamplePartial {
            ok: ok_trials,
            last_err: e,
        },
        (_, None) => Verdict::NoCounterexample,
    }
}

#[cfg(test)]
mod tests {
    use super::{test_pair, Config, Verdict};

    const CFG: Config = Config {
        trials: 4,
        nrows: 5,
        seed: 0,
    };

    /// A query on one side and a mutation on the other are compared by table state, an observable
    /// the query side does not have -- so the comparison would answer a question about neither
    /// statement. It has to be refused before the observable is chosen.
    #[test]
    fn a_query_paired_with_a_mutation_is_not_comparable() {
        let v = test_pair(
            "SELECT * FROM t WHERE a = 1",
            "DELETE FROM t WHERE a = 1",
            "CREATE TABLE t (a int)",
            CFG,
        );
        match &v {
            Verdict::NotComparable(d) => assert_eq!(d, "mixed-kind: query vs mutation"),
            other => panic!("{other:?}"),
        }
        // Both orders, because the guard is a disagreement and not a property of the second side.
        let v = test_pair(
            "DELETE FROM t WHERE a = 1",
            "SELECT * FROM t WHERE a = 1",
            "CREATE TABLE t (a int)",
            CFG,
        );
        match &v {
            Verdict::NotComparable(d) => assert_eq!(d, "mixed-kind: mutation vs query"),
            other => panic!("{other:?}"),
        }
    }

    /// The guard must not reach an ordinary pair: two mutations share the table-state observable
    /// and two queries share the result-set one.
    #[test]
    fn same_kind_pairs_are_still_compared() {
        for (a, b) in [
            ("DELETE FROM t WHERE a = 1", "DELETE FROM t WHERE 1 = a"),
            ("SELECT * FROM t WHERE a = 1", "SELECT * FROM t WHERE 1 = a"),
        ] {
            let v = test_pair(a, b, "CREATE TABLE t (a int)", CFG);
            assert!(matches!(v, Verdict::NoCounterexample), "{a} / {b}: {v:?}");
        }
    }

    /// The observable the tester used to be blind to. Both sides leave the table byte-identical --
    /// `SET a = a` writes each row's own value back -- so a table-state comparison reports
    /// `no-counterexample` on a pair that is plainly not equivalent: one hands the caller every row
    /// and the other hands back nothing. Observing the returned bag is what turns it into the
    /// refutation it always was.
    #[test]
    fn a_pair_differing_only_in_the_returned_bag_is_refuted() {
        let v = test_pair(
            "UPDATE t SET a = a WHERE true RETURNING a",
            "UPDATE t SET a = a WHERE false RETURNING a",
            "CREATE TABLE t (a int)",
            CFG,
        );
        assert!(matches!(v, Verdict::NotEquivalent(_)), "{v:?}");
    }

    /// ...and the bag must not manufacture a difference where there is none. Both sides delete the
    /// same rows and return the same projection of them; `RETURNING` fixes no row order, so the bag
    /// is compared sorted.
    #[test]
    fn an_identical_returning_pair_is_still_no_counterexample() {
        for (a, b) in [
            (
                "DELETE FROM t WHERE a = 1 RETURNING a",
                "DELETE FROM t WHERE 1 = a RETURNING a",
            ),
            (
                "INSERT INTO t VALUES (1), (2) RETURNING a",
                "INSERT INTO t VALUES (2), (1) RETURNING a",
            ),
            (
                "UPDATE t SET a = a + 1 WHERE a > 0 RETURNING *",
                "UPDATE t SET a = 1 + a WHERE 0 < a RETURNING *",
            ),
        ] {
            let v = test_pair(a, b, "CREATE TABLE t (a int)", CFG);
            assert!(matches!(v, Verdict::NoCounterexample), "{a} / {b}: {v:?}");
        }
    }

    /// One side returning a bag and the other returning none is the mixed-kind problem one level
    /// down: an observable only one side has.
    #[test]
    fn a_one_sided_returning_pair_is_not_comparable() {
        let v = test_pair(
            "DELETE FROM t WHERE a = 1 RETURNING a",
            "DELETE FROM t WHERE 1 = a",
            "CREATE TABLE t (a int)",
            CFG,
        );
        match &v {
            Verdict::NotComparable(d) => assert_eq!(d, "one-sided RETURNING: returning vs none"),
            other => panic!("{other:?}"),
        }
        // Both orders: the guard is a disagreement, not a property of the second side.
        let v = test_pair(
            "DELETE FROM t WHERE a = 1",
            "DELETE FROM t WHERE 1 = a RETURNING a",
            "CREATE TABLE t (a int)",
            CFG,
        );
        match &v {
            Verdict::NotComparable(d) => assert_eq!(d, "one-sided RETURNING: none vs returning"),
            other => panic!("{other:?}"),
        }
    }

    /// The word `returning` is not always the clause: it can be a quoted column name or sit inside a
    /// string literal. Deciding from the text rather than the AST would refuse both of these pairs
    /// as one-sided, and would make the tester fetch rows from a statement that returns none.
    /// (Unquoted it is not available as a column name -- DuckDB's parser rejects
    /// `WHERE returning = 1` outright -- so the quoted spelling is the only shape that reaches us.)
    #[test]
    fn the_word_returning_in_a_name_or_a_literal_is_not_the_clause() {
        let v = test_pair(
            "DELETE FROM t WHERE \"returning\" = 1",
            "DELETE FROM t WHERE 1 = \"returning\"",
            "CREATE TABLE t (\"returning\" int, note text)",
            CFG,
        );
        assert!(matches!(v, Verdict::NoCounterexample), "{v:?}");
        let v = test_pair(
            "DELETE FROM t WHERE note = \'returning\'",
            "DELETE FROM t WHERE \'returning\' = note",
            "CREATE TABLE t (\"returning\" int, note text)",
            CFG,
        );
        assert!(matches!(v, Verdict::NoCounterexample), "{v:?}");
    }
}
