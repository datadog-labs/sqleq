// Unless explicitly stated otherwise all files in this repository are licensed under the
// Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/).
// Copyright 2026-Present Datadog, Inc.

//! Translate `INSERT` equivalence pairs into Lean 4 and check them with the Lean kernel.
//!
//! The Lean side lives in the repository's `lean/` package. This crate parses a pair with the
//! frontend's own parser ([`sqleq_frontend::internals::parse_pair`]), recognises the shapes the
//! Lean checker decides ([`recognize`], [`translate`](mod@translate)), emits one `.lean` file per
//! batch of pairs ([`emit`]), and runs `lake env lean` on it ([`run`]).
//!
//! What a proof here means is stated in `lean/Sqleq/Check.lean` (`EquivGather`) and in
//! `docs/LEAN.md`.

pub mod emit;
pub mod recognize;
pub mod run;
pub mod schema;
pub mod translate;

use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;
use std::time::Instant;

use serde_json::{json, Value};
use sqlparser::ast::Statement;

use recognize::Refusal;
use schema::Schema;
use translate::{translate, LeanPair};

/// The kernel proved `EquivGather`, and the `VALUES` side's canonical run succeeds (see
/// `Sqleq.Witness`). The credited verdict.
pub const PROVED: &str = "proved-gather";

/// The kernel proved `EquivGather`, but no witness shows the `VALUES` side can succeed, so the proof
/// may be vacuous (both sides always error). Not credited.
pub const NO_WITNESS: &str = "no-witness";

/// One pair's input, already parsed, and as the original text for the Postgres replay.
pub struct Case {
    pub name: String,
    pub pair: Result<(Statement, Statement), Refusal>,
    pub schema: Schema,
    /// The DDL as statements of original text.
    pub ddl: Vec<String>,
    /// The two statements as the input writes them, in input order.
    pub sql: (String, String),
}

impl Case {
    /// A pair file: optional DDL, then exactly two statements, which are the pair.
    pub fn from_file(name: String, text: &str) -> Case {
        let mut parts = sqleq_frontend::pgddl::split_ddl(text);
        let sql = match (parts.pop(), parts.pop()) {
            (Some(b), Some(a)) => (a, b),
            _ => (String::new(), String::new()),
        };
        match sqleq_frontend::internals::parse_pair(text) {
            Err(e) => Case {
                name,
                pair: Err(Refusal::Unsupported(format!("parse: {e}"))),
                schema: Schema::default(),
                ddl: parts,
                sql,
            },
            Ok(mut st) => {
                let schema = Schema::from_statements(st.iter().map(|s| (s, false)));
                let pair = if st.len() < 2 {
                    Err(Refusal::Unsupported("expected two statements".into()))
                } else {
                    let b = st.pop().unwrap();
                    let a = st.pop().unwrap();
                    Ok((a, b))
                };
                Case { name, pair, schema, ddl: parts, sql }
            }
        }
    }

    /// A corpus row: each side is one statement, and the DDL comes separately.
    pub fn from_row(name: String, a: &str, b: &str, ddl: Option<&str>) -> Case {
        let one = |s: &str| -> Result<Statement, Refusal> {
            let mut v = sqleq_frontend::internals::parse_pair(s)
                .map_err(|e| Refusal::Unsupported(format!("parse: {e}")))?;
            if v.len() != 1 {
                return Err(Refusal::Unsupported("a side is not exactly one statement".into()));
            }
            Ok(v.remove(0))
        };
        let pair = one(a).and_then(|x| one(b).map(|y| (x, y)));
        Case {
            name,
            pair,
            schema: ddl.map(Schema::from_ddl).unwrap_or_default(),
            ddl: ddl.map(sqleq_frontend::pgddl::split_ddl).unwrap_or_default(),
            sql: (a.trim().trim_end_matches(';').to_string(), b.trim().trim_end_matches(';').to_string()),
        }
    }
}

/// What the Postgres replay needs for one checked pair: the DDL, both statements as written, the
/// `VALUES` rows (so it can build the canonical and gather bindings), and each table column's type
/// and default as the DDL writes them.
pub fn replay_plan(case: &Case, p: &LeanPair) -> Value {
    let rows: Vec<Vec<Value>> = match &p.a.src {
        translate::LSource::Values(rows) => rows
            .iter()
            .map(|r| {
                r.iter()
                    .map(|c| match c {
                        translate::LCell::P(n) | translate::LCell::Pc(n, _) => json!(n),
                        translate::LCell::Null => Value::Null,
                    })
                    .collect()
            })
            .collect(),
        translate::LSource::Unnest(_) => Vec::new(),
    };
    let (values_sql, unnest_sql) =
        if p.flipped { (&case.sql.1, &case.sql.0) } else { (&case.sql.0, &case.sql.1) };
    json!({
        "ddl": case.ddl,
        "values_sql": values_sql,
        "unnest_sql": unnest_sql,
        "target": p.replay.target_sql,
        "insert": p.replay.insert,
        "rows": rows,
        "columns": p.replay.columns.iter().map(|(n, t, d, listed)| json!({
            "name": n, "type": t, "default": d, "listed": listed,
        })).collect::<Vec<_>>(),
    })
}

/// Pairs with more `VALUES` rows than this are checked one per file.
const LARGE_ROWS: usize = 100;

/// Run Lean on each file, `jobs` at a time. `None` for a file that timed out.
fn run_files(lean: &run::Lean, files: &[std::path::PathBuf], jobs: usize) -> Result<Vec<Option<String>>, String> {
    let out: Mutex<Vec<Option<String>>> = Mutex::new(vec![None; files.len()]);
    let next = AtomicUsize::new(0);
    let failure: Mutex<Option<String>> = Mutex::new(None);
    std::thread::scope(|s| {
        for _ in 0..jobs.max(1) {
            s.spawn(|| loop {
                let i = next.fetch_add(1, Ordering::SeqCst);
                let Some(f) = files.get(i) else { break };
                match lean.check(f) {
                    Ok(o) => out.lock().unwrap()[i] = o,
                    Err(e) => {
                        *failure.lock().unwrap() = Some(e);
                        break;
                    }
                }
            });
        }
    });
    match failure.into_inner().unwrap() {
        Some(e) => Err(e),
        None => Ok(out.into_inner().unwrap()),
    }
}

/// Words for a failed canonical run, from `Sqleq.WResult`'s printed form.
fn explain(p: &LeanPair, w: &str) -> String {
    let mut it = w.split_whitespace();
    let tag = it.next().unwrap_or("");
    let arg: Option<usize> = it.next().and_then(|n| n.parse().ok());
    let col = |i: Option<usize>| i.and_then(|i| p.col_names.get(i)).cloned().unwrap_or_else(|| "?".into());
    let uniq = |i: Option<usize>| i.and_then(|i| p.unique_names.get(i)).cloned().unwrap_or_else(|| "?".into());
    let why = match tag {
        "notNull" => format!("column {} is NOT NULL and gets NULL", col(arg)),
        "unique" => format!("two of its rows collide on {}", uniq(arg)),
        "secondTime" => format!("ON CONFLICT DO UPDATE would update a row the same statement inserted, on {}", uniq(arg)),
        "generated" => format!("it gives GENERATED ALWAYS column {} a value", col(arg)),
        "noArbiter" => "no unique constraint matches its conflict target".into(),
        "nothingInserted" => "it inserts no row".into(),
        other => format!("the canonical run ends in `{other}`"),
    };
    format!("the VALUES side fails every run with non-NULL parameters: {why}")
}

/// Check every case; returns one JSON record per case, in input order.
pub fn check(
    cases: &[Case],
    lean: &run::Lean,
    batch: usize,
    jobs: usize,
    keep: Option<&Path>,
    with_plan: bool,
) -> Result<Vec<(String, Value)>, String> {
    let translated: Vec<Result<LeanPair, Refusal>> = cases
        .iter()
        .map(|c| match &c.pair {
            Err(r) => Err(r.clone()),
            Ok((a, b)) => translate(a, b, &c.schema),
        })
        .collect();
    let pair = |i: usize| translated[i].as_ref().unwrap();
    // A large pair gets a batch of its own, so that if it is slow it cannot time out others.
    let rows = |i: usize| match &pair(i).a.src {
        translate::LSource::Values(r) => r.len(),
        translate::LSource::Unnest(_) => 0,
    };
    let (large, small): (Vec<usize>, Vec<usize>) =
        (0..cases.len()).filter(|&i| translated[i].is_ok()).partition(|&i| rows(i) > LARGE_ROWS);
    let todo: Vec<usize> = small.iter().chain(&large).copied().collect();
    let mut batches: Vec<&[usize]> = small.chunks(batch.max(1)).collect();
    batches.extend(large.chunks(1));

    let dir = match keep {
        Some(d) => d.to_path_buf(),
        None => std::env::temp_dir().join(format!("sqleq-lean-{}", std::process::id())),
    };
    std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    if !todo.is_empty() {
        lean.build()?;
    }

    // Pass 1: every proof and every witness.
    let mut files = Vec::new();
    let mut layouts = Vec::new();
    for (bi, members) in batches.iter().enumerate() {
        let pairs: Vec<&LeanPair> = members.iter().map(|&i| pair(i)).collect();
        let (src, entries) = emit::batch(&pairs);
        let file = dir.join(format!("batch{bi:05}.lean"));
        std::fs::write(&file, src).map_err(|e| format!("{}: {e}", file.display()))?;
        files.push(file);
        layouts.push(entries);
    }
    let start = Instant::now();
    let outputs = run_files(lean, &files, jobs)?;
    // Approximate Lean CPU time per pair: wall time times workers, spread over the pairs.
    let ms = start.elapsed().as_millis() as u64 * jobs.max(1) as u64 / todo.len().max(1) as u64;
    // (proof outcome, witness outcome) per case; `None` when the batch timed out.
    let mut got: Vec<Option<(run::Outcome, Option<run::Outcome>)>> = vec![None; cases.len()];
    for (((members, entries), out), file) in batches.iter().zip(&layouts).zip(&outputs).zip(&files) {
        let Some(text) = out else { continue };
        // The audit reads the file Lean read, not the string that was written to it.
        let proofs: Vec<_> = entries.iter().map(|e| e.proof.clone()).collect();
        let src = std::fs::read_to_string(file).map_err(|e| format!("{}: {e}", file.display()))?;
        let po = run::outcomes(&src, text, &proofs);
        for ((&i, e), p) in members.iter().zip(entries).zip(po) {
            let w = e.witness.as_ref().map(|w| run::outcomes(&src, text, std::slice::from_ref(w)).remove(0));
            got[i] = Some((p, w));
        }
    }

    // Pass 2: why each failed witness failed.
    let failed: Vec<usize> = (0..cases.len())
        .filter(|&i| matches!(&got[i], Some((run::Outcome::Proved, Some(run::Outcome::Failed(_))))))
        .collect();
    let mut why: std::collections::HashMap<usize, String> = std::collections::HashMap::new();
    let chunks: Vec<&[usize]> = failed.chunks(batch.max(1)).collect();
    let mut rfiles = Vec::new();
    for (bi, members) in chunks.iter().enumerate() {
        let tagged: Vec<(usize, &LeanPair)> = members.iter().map(|&i| (i, pair(i))).collect();
        let file = dir.join(format!("reasons{bi:05}.lean"));
        std::fs::write(&file, emit::reasons(&tagged)).map_err(|e| format!("{}: {e}", file.display()))?;
        rfiles.push(file);
    }
    for out in run_files(lean, &rfiles, jobs)?.into_iter().flatten() {
        why.extend(run::reduced(&out));
    }
    // A reason can go missing when a batch file fails as a whole (seen once, under load, and not
    // reproducible alone). Retry those pairs one per file. The verdict does not depend on this:
    // the kernel already refused the witness. Only its explanation does.
    let missing: Vec<usize> = failed.iter().copied().filter(|i| !why.contains_key(i)).collect();
    let mut retry = Vec::new();
    for &i in &missing {
        let file = dir.join(format!("reason-retry{i}.lean"));
        std::fs::write(&file, emit::reasons(&[(i, pair(i))])).map_err(|e| format!("{}: {e}", file.display()))?;
        retry.push(file);
    }
    for out in run_files(lean, &retry, jobs)?.into_iter().flatten() {
        why.extend(run::reduced(&out));
    }

    if keep.is_none() {
        let _ = std::fs::remove_dir_all(&dir);
    }
    Ok(cases
        .iter()
        .zip(&translated)
        .enumerate()
        .map(|(i, (c, t))| {
            let rec = match (t, &got[i]) {
                (Err(refusal), _) => {
                    json!({"verdict": refusal.verdict(), "reason": refusal.reason(), "ms": 0})
                }
                (Ok(p), g) => {
                    let (verdict, reason) = match g {
                        None => ("timeout".to_string(), None),
                        Some((run::Outcome::Failed(m), _)) => ("error".to_string(), Some(m.clone())),
                        Some((run::Outcome::Proved, w)) => match (&p.witness, w) {
                            (translate::Witness::Skip(r), _) => (NO_WITNESS.to_string(), Some(r.clone())),
                            (_, Some(run::Outcome::Proved)) => (PROVED.to_string(), None),
                            (_, _) => (
                                NO_WITNESS.to_string(),
                                Some(match why.get(&i) {
                                    Some(w) if !w.starts_with("ok") => explain(p, w),
                                    Some(w) => format!("witness: the kernel refused a run the model evaluates to `{w}`"),
                                    None => "the VALUES side's canonical run fails".to_string(),
                                }),
                            ),
                        },
                    };
                    let mut v = json!({
                        "verdict": verdict, "shape": p.shape.name(), "flipped": p.flipped, "ms": ms
                    });
                    if let Some(r) = reason {
                        v["reason"] = json!(r);
                    }
                    if !p.unmodelled.is_empty() {
                        v["unmodelled"] = json!(p.unmodelled);
                    }
                    if with_plan && (verdict == PROVED || verdict == NO_WITNESS) {
                        v["replay"] = replay_plan(c, p);
                    }
                    v
                }
            };
            (c.name.clone(), rec)
        })
        .collect())
}
