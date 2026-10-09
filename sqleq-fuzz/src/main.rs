// Unless explicitly stated otherwise all files in this repository are licensed under the
// Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/).
// Copyright 2026-Present Datadog, Inc.

//! CLI for the concrete differential tester. `sqleq-check` normally runs it, as its `fuzz` axis.
//!
//! Modes:
//!   * `sqleq-fuzz csv <corpus.csv> <names.txt> [out.json]` — batch a corpus (rows are `a,b,ddl`);
//!     `names.txt` lists `pairNNNN` entries (the digits index a corpus row). Parallel with `--jobs`.
//!     `out.json` defaults to the corpus path with its extension replaced by `.fuzz.json`.
//!   * `sqleq-fuzz row <corpus.csv> <index>` — test a single corpus row and print the verdict.
//!   * `sqleq-fuzz file <pair.sql>` — test a self-contained file (CREATE and ALTER statements + exactly
//!     two statements).
//!
//! Options: `--jobs N` (or `-j N`), `--trials N`, `--rows N`, `--seed N`, and `--engine duckdb|postgres`
//! (default `$SQLEQ_FUZZ_ENGINE`, else `postgres`; see `sqleq_fuzz::pg`). Any verdict exits 0; an
//! input that cannot be read, or a missing argument, exits 1; no mode, or an unknown one, prints the
//! usage and exits 2. A panic while testing one pair is that pair's `ERROR:panic: …` verdict, and in
//! `csv` mode the worker goes on to the next row.

use std::collections::BTreeMap;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::Path;
use std::process::ExitCode;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use sqleq_fuzz::{pg, test_pair, Config, Verdict};

const USAGE: &str = "\
usage:
  sqleq-fuzz csv  <corpus.csv> <names.txt> [out.json]   batch a corpus (out.json defaults to the
                                                        corpus path with extension .fuzz.json)
  sqleq-fuzz row  <corpus.csv> <index>                  test one corpus row (counting from 0)
  sqleq-fuzz file <pair.sql>                            test a file: DDL, then two statements
options:
  -j, --jobs N   parallel workers (csv mode, default 1)
  --trials N     random instances per pair (default 120)
  --rows N       rows per table per instance (default 5)
  --seed N       RNG seed (default 0)
  --engine E     where the statements run: postgres (default; $SQLEQ_FUZZ_ENGINE overrides), a
                 private PostgreSQL 17 cluster from $SQLEQ_PG_BIN or PATH, or duckdb";

fn main() -> ExitCode {
    let raw: Vec<String> = std::env::args().skip(1).collect();
    let mut cfg = Config::default();
    let mut jobs = 1usize;
    let mut engine = std::env::var("SQLEQ_FUZZ_ENGINE").unwrap_or_else(|_| "postgres".to_string());
    let mut pos: Vec<String> = Vec::new();

    let mut i = 0;
    while i < raw.len() {
        match raw[i].as_str() {
            "--jobs" | "-j" => {
                i += 1;
                jobs = raw.get(i).and_then(|v| v.parse().ok()).unwrap_or(jobs);
            }
            "--trials" => {
                i += 1;
                cfg.trials = raw
                    .get(i)
                    .and_then(|v| v.parse().ok())
                    .unwrap_or(cfg.trials);
            }
            "--rows" => {
                i += 1;
                cfg.nrows = raw.get(i).and_then(|v| v.parse().ok()).unwrap_or(cfg.nrows);
            }
            "--engine" => {
                i += 1;
                engine = raw.get(i).cloned().unwrap_or_default();
            }
            "--seed" => {
                i += 1;
                cfg.seed = raw.get(i).and_then(|v| v.parse().ok()).unwrap_or(cfg.seed);
            }
            "-h" | "--help" => {
                println!("{USAGE}");
                return ExitCode::SUCCESS;
            }
            other => pos.push(other.to_string()),
        }
        i += 1;
    }

    if engine != "duckdb" && engine != "postgres" {
        eprintln!("unknown engine {engine:?}: expected duckdb or postgres\n{USAGE}");
        return ExitCode::from(2);
    }
    let result = match pos.first().map(String::as_str) {
        Some("csv") => run_csv(&pos[1..], cfg, jobs.max(1), &engine),
        Some("row") => run_row(&pos[1..], cfg, &engine),
        Some("file") => run_file(&pos[1..], cfg, &engine),
        _ => {
            eprintln!("{USAGE}");
            return ExitCode::from(2);
        }
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}

/// Load a corpus CSV as `(a, b, ddl)` rows (no header; ddl optional).
fn load_corpus(path: &str) -> Result<Vec<(String, String, String)>, String> {
    let mut rdr = csv::ReaderBuilder::new()
        .has_headers(false)
        .flexible(true)
        .from_path(path)
        .map_err(|e| format!("cannot read {path}: {e}"))?;
    let mut out = Vec::new();
    for rec in rdr.records() {
        let rec = rec.map_err(|e| format!("csv parse error in {path}: {e}"))?;
        let get = |i: usize| rec.get(i).unwrap_or("").to_string();
        out.push((get(0), get(1), get(2)));
    }
    Ok(out)
}

/// Test one pair, turning a panic into that pair's `ERROR` verdict instead of the process's end.
///
/// A panic is a defect in this crate, never a fact about the pair, so it must not read as a verdict
/// about it; but it must not take other rows down with it either. In `csv` mode an uncaught panic
/// killed the worker thread that drew the row, and with it every row that worker would have run
/// next, while the run still exited 0.
fn guarded_test(a: &str, b: &str, ddl: &str, cfg: Config) -> Verdict {
    guarded(|| test_pair(a, b, ddl, cfg))
}

/// Run `f`, turning a panic into an `ERROR:panic: …` verdict.
fn guarded(f: impl FnOnce() -> Verdict) -> Verdict {
    catch_unwind(AssertUnwindSafe(f)).unwrap_or_else(|payload| {
        let msg = payload
            .downcast_ref::<String>()
            .map(String::as_str)
            .or_else(|| payload.downcast_ref::<&str>().copied())
            .unwrap_or("unknown panic");
        Verdict::Error(format!("panic: {}", msg.lines().next().unwrap_or("")))
    })
}

/// Where the pairs run: in-process DuckDB, or a private Postgres cluster with a database per worker.
enum Runner {
    /// DuckDB's version, as `SELECT version()` reports it.
    Duck(String),
    Pg(pg::Server),
}

impl Runner {
    fn start(engine: &str, workers: usize) -> Result<Runner, String> {
        match engine {
            "postgres" => Ok(Runner::Pg(pg::Server::start(workers)?)),
            _ => {
                let version = sqleq_fuzz::duck::open_db()
                    .and_then(|c| c.query_row("SELECT version()", [], |r| r.get::<_, String>(0)))
                    .map_err(|e| format!("cannot open DuckDB: {e}"))?;
                Ok(Runner::Duck(version.trim_start_matches('v').to_string()))
            }
        }
    }

    /// `postgres 17.11` or `duckdb 1.5.5`: the engine a verdict is a claim about. A DuckDB verdict is
    /// about DuckDB's semantics, which is why it always says so.
    fn engine(&self) -> String {
        match self {
            Runner::Duck(v) => format!("duckdb {v}"),
            Runner::Pg(s) => format!("postgres {}", s.version()),
        }
    }

    fn worker(&self, i: usize) -> Result<Worker, String> {
        match self {
            Runner::Duck(_) => Ok(Worker::Duck),
            Runner::Pg(s) => Ok(Worker::Pg(Box::new(s.worker(i)?))),
        }
    }
}

enum Worker {
    Duck,
    Pg(Box<postgres::Client>),
}

impl Worker {
    /// The pair's verdict, and what it rests on beyond the DDL as written ([`pg::Timing::caveat`]).
    fn test(&mut self, a: &str, b: &str, ddl: &str, cfg: Config) -> (Verdict, Option<String>) {
        match self {
            Worker::Duck => (guarded_test(a, b, ddl, cfg), None),
            Worker::Pg(client) => {
                let mut caveat = None;
                let v = guarded(|| {
                    let o = pg::test_pair_pg(client, a, b, ddl, cfg);
                    caveat = o.timing.caveat();
                    o.verdict
                });
                if v.label().starts_with("ERROR:panic") {
                    // A panic can leave the pair's transaction open.
                    let _ = client.batch_execute("ROLLBACK");
                }
                (v, caveat)
            }
        }
    }
}

/// Where `csv` mode writes when no `out.json` is given: beside the corpus, as `<corpus>.fuzz.json`.
/// A fixed path elsewhere would let two runs overwrite each other.
fn default_out(corpus_path: &str) -> String {
    Path::new(corpus_path)
        .with_extension("fuzz.json")
        .to_string_lossy()
        .into_owned()
}

fn run_csv(args: &[String], cfg: Config, jobs: usize, engine: &str) -> Result<(), String> {
    let corpus_path = args.first().ok_or("csv mode needs <corpus.csv>")?;
    let names_path = args.get(1).ok_or("csv mode needs <names.txt>")?;
    let out_path = args
        .get(2)
        .cloned()
        .unwrap_or_else(|| default_out(corpus_path));

    let corpus = Arc::new(load_corpus(corpus_path)?);
    let runner = Arc::new(Runner::start(engine, jobs)?);
    let engine_tag = runner.engine();
    let names_txt = std::fs::read_to_string(names_path)
        .map_err(|e| format!("cannot read {names_path}: {e}"))?;
    // Each name maps to a corpus row via its trailing digits: `pairNNNN` is row NNNN.
    let work: Vec<(String, Option<usize>)> = names_txt
        .lines()
        .map(|l| l.trim())
        .filter(|l| !l.is_empty())
        .map(|l| {
            let digits: String = l.chars().filter(|c| c.is_ascii_digit()).collect();
            (l.to_string(), digits.parse::<usize>().ok())
        })
        .collect();
    let work = Arc::new(work);

    // name -> (verdict label, Some((ok trials, last error)) when only some trials ran, wall ms,
    // what the verdict rests on beyond the DDL)
    type Row = (String, Option<(usize, String)>, u128, Option<String>);
    let results: Arc<Mutex<BTreeMap<String, Row>>> = Arc::new(Mutex::new(BTreeMap::new()));
    let next = Arc::new(AtomicUsize::new(0));

    let mut handles = Vec::new();
    for w in 0..jobs {
        let corpus = Arc::clone(&corpus);
        let work = Arc::clone(&work);
        let results = Arc::clone(&results);
        let next = Arc::clone(&next);
        let mut worker = runner.worker(w)?;
        handles.push(std::thread::spawn(move || loop {
            let idx = next.fetch_add(1, Ordering::Relaxed);
            if idx >= work.len() {
                break;
            }
            let (name, row) = &work[idx];
            let started = std::time::Instant::now();
            let (label, partial, caveat) = match row.and_then(|r| corpus.get(r)) {
                Some((a, b, ddl)) => {
                    let (v, caveat) = worker.test(a, b, ddl, cfg);
                    let p = v.partial().map(|(ok, e)| (ok, e.to_string()));
                    (v.label(), p, caveat)
                }
                None => ("NO-ROW".to_string(), None, None),
            };
            let ms = started.elapsed().as_millis();
            match &partial {
                Some((ok, _)) => {
                    println!("{name}: {label} (ok={ok}/{}) {ms}ms", cfg.total_trials())
                }
                None => println!("{name}: {label} {ms}ms"),
            }
            results
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .insert(name.clone(), (label, partial, ms, caveat));
        }));
    }
    // A worker can only die now through a defect outside `test_pair`; its unfinished rows would be
    // missing from the output, so that is the run's failure rather than a quiet gap.
    let died = handles.into_iter().map(|h| h.join()).filter(Result::is_err).count();

    // Write `{name: {"verdict": label, "ms": wall}}` (a superset of the Python tester's output
    // shape). Partially-run pairs carry two extra keys; consumers that only read "verdict" are
    // unaffected.
    let map = results.lock().unwrap_or_else(|e| e.into_inner());
    let json: serde_json::Map<String, serde_json::Value> = map
        .iter()
        .map(|(k, (label, partial, ms, caveat))| {
            let mut o = serde_json::Map::new();
            o.insert("verdict".to_string(), serde_json::json!(label));
            o.insert("ms".to_string(), serde_json::json!(ms));
            o.insert("engine".to_string(), serde_json::json!(engine_tag));
            if let Some(c) = caveat {
                o.insert("caveat".to_string(), serde_json::json!(c));
            }
            if let Some((ok, err)) = partial {
                o.insert("ok_trials".to_string(), serde_json::json!(ok));
                o.insert("trial_error".to_string(), serde_json::json!(err));
            }
            (k.clone(), serde_json::Value::Object(o))
        })
        .collect();
    std::fs::write(
        &out_path,
        serde_json::to_string(&serde_json::Value::Object(json)).unwrap(),
    )
    .map_err(|e| format!("cannot write {out_path}: {e}"))?;
    eprintln!("wrote {} verdicts to {out_path}", map.len());
    if died > 0 {
        return Err(format!(
            "{died} worker(s) died; {} of {} rows have no verdict",
            work.len() - map.len(),
            work.len()
        ));
    }
    Ok(())
}

fn run_row(args: &[String], cfg: Config, engine: &str) -> Result<(), String> {
    let corpus_path = args.first().ok_or("row mode needs <corpus.csv>")?;
    let idx: usize = args
        .get(1)
        .ok_or("row mode needs <index>")?
        .parse()
        .map_err(|_| "index must be a number")?;
    let corpus = load_corpus(corpus_path)?;
    let (a, b, ddl) = corpus
        .get(idx)
        .ok_or(format!("row {idx} out of range (len {})", corpus.len()))?;
    test_one(a, b, ddl, cfg, engine)
}

fn run_file(args: &[String], cfg: Config, engine: &str) -> Result<(), String> {
    let path = args.first().ok_or("file mode needs <pair.sql>")?;
    let src = std::fs::read_to_string(path).map_err(|e| format!("cannot read {path}: {e}"))?;
    // Drop the frontend's `declare ... function` DSL lines (not runnable SQL), then split into
    // statements: CREATE*, ALTER* and DROP* form the DDL, the remaining two are the query pair. An
    // `ALTER TABLE ... ADD PRIMARY KEY` or a `DROP INDEX` is as much a part of the schema as the
    // CREATE it alters.
    let cleaned: String = src
        .lines()
        .filter(|l| {
            let t = l.trim_start().to_lowercase();
            !(t.starts_with("declare ") && t.contains("function"))
        })
        .collect::<Vec<_>>()
        .join("\n");
    let mut ddl_parts: Vec<String> = Vec::new();
    let mut queries: Vec<String> = Vec::new();
    for s in split_statements(&cleaned) {
        let upper = s.to_uppercase();
        if upper.starts_with("CREATE") || upper.starts_with("ALTER") || upper.starts_with("DROP") {
            ddl_parts.push(s);
        } else {
            queries.push(s);
        }
    }
    if queries.len() != 2 {
        return Err(format!(
            "expected exactly 2 statements besides the DDL, got {}",
            queries.len()
        ));
    }
    let ddl = ddl_parts.join(";\n");
    test_one(&queries[0], &queries[1], &ddl, cfg, engine)
}

/// Split a file into statements on top-level `;`, skipping semicolons inside string literals,
/// quoted identifiers and comments. Each statement comes back with its leading comments removed, so
/// the caller's `CREATE` test sees the keyword rather than the `--` line above it; text that is only
/// a comment yields no statement at all.
fn split_statements(src: &str) -> Vec<String> {
    let b = src.as_bytes();
    let mut out: Vec<String> = Vec::new();
    let (mut start, mut i) = (0usize, 0usize);
    while i < b.len() {
        match b[i] {
            // A doubled quote inside a quoted run is an escaped quote, not the terminator.
            q @ (b'\'' | b'"') => {
                i += 1;
                while i < b.len() {
                    if b[i] == q {
                        if b.get(i + 1) == Some(&q) {
                            i += 2;
                            continue;
                        }
                        break;
                    }
                    i += 1;
                }
                i += 1;
            }
            b'-' if b.get(i + 1) == Some(&b'-') => {
                while i < b.len() && b[i] != b'\n' {
                    i += 1;
                }
            }
            b'/' if b.get(i + 1) == Some(&b'*') => {
                i += 2;
                while i < b.len() && !(b[i] == b'*' && b.get(i + 1) == Some(&b'/')) {
                    i += 1;
                }
                i = (i + 2).min(b.len());
            }
            b';' => {
                push_statement(&mut out, &src[start..i]);
                i += 1;
                start = i;
            }
            _ => i += 1,
        }
    }
    push_statement(&mut out, &src[start..]);
    out
}

/// Trim a statement and drop its leading comments; push it unless nothing is left.
fn push_statement(out: &mut Vec<String>, raw: &str) {
    let mut s = raw;
    loop {
        s = s.trim_start();
        if let Some(rest) = s.strip_prefix("--") {
            s = rest.split_once('\n').map_or("", |(_, tail)| tail);
        } else if let Some(rest) = s.strip_prefix("/*") {
            s = rest.split_once("*/").map_or("", |(_, tail)| tail);
        } else {
            break;
        }
    }
    let s = s.trim_end();
    if !s.is_empty() {
        out.push(s.to_string());
    }
}

/// Test one pair on `engine` and print its report.
fn test_one(a: &str, b: &str, ddl: &str, cfg: Config, engine: &str) -> Result<(), String> {
    let runner = Runner::start(engine, 1)?;
    let (v, caveat) = runner.worker(0)?.test(a, b, ddl, cfg);
    report(&v, &runner.engine(), caveat.as_deref());
    Ok(())
}

fn report(v: &Verdict, engine: &str, caveat: Option<&str>) {
    println!("{}", v.label());
    if let Verdict::NotEquivalent(ce) = v {
        println!("counterexample: {ce}");
    }
    // The same fact `csv` mode records as `ok_trials` / `trial_error`: the label is the finding,
    // and this says how thin the coverage behind it was. A line of its own, so a reader of the
    // first line sees what it always saw.
    if let Some((ok, err)) = v.partial() {
        println!("partial: {ok} trials compared both sides; last error: {err}");
    }
    println!("engine: {engine}");
    if let Some(c) = caveat {
        println!("caveat: {c}");
    }
}

#[cfg(test)]
mod tests {
    use super::{default_out, guarded, split_statements};
    use sqleq_fuzz::Verdict;

    /// A panic inside one pair's test is that pair's `ERROR` verdict. It used to unwind through the
    /// `csv` worker that drew the row, and every row that worker would have run next went missing.
    #[test]
    fn a_panic_is_the_rows_error_verdict() {
        match guarded(|| panic!("boom")) {
            Verdict::Error(e) => assert_eq!(e, "panic: boom"),
            other => panic!("{other:?}"),
        }
        match guarded(|| panic!("{} went wrong", 1 + 1)) {
            Verdict::Error(e) => assert_eq!(e, "panic: 2 went wrong"),
            other => panic!("{other:?}"),
        }
        assert!(matches!(guarded(|| Verdict::NoSchema), Verdict::NoSchema));
    }

    #[test]
    fn the_default_output_sits_beside_the_corpus() {
        assert_eq!(default_out("runs/corpus.csv"), "runs/corpus.fuzz.json");
        assert_eq!(default_out("corpus"), "corpus.fuzz.json");
    }

    /// The shape of `examples/*.sql`: a comment above the DDL used to hide `CREATE` from the
    /// classifier, so the schema was counted as a third query and the file was rejected.
    #[test]
    fn leading_comment_does_not_hide_the_keyword() {
        let src = "-- Schema for the pair.\ncreate table \"t\" (\"id\" INTEGER);\n\
                   -- A: one way\nSELECT 1;\n-- B: the other\nSELECT 1;\n";
        let stmts = split_statements(src);
        assert_eq!(stmts.len(), 3);
        assert!(stmts[0].to_uppercase().starts_with("CREATE"));
        assert_eq!(stmts[1], "SELECT 1");
        assert_eq!(stmts[2], "SELECT 1");
    }

    #[test]
    fn semicolons_inside_quotes_and_comments_do_not_split() {
        let stmts = split_statements(
            "SELECT 'a;b' AS \"c;d\"; -- trailing ; note\nSELECT 2; /* block ; */\n",
        );
        assert_eq!(stmts, vec!["SELECT 'a;b' AS \"c;d\"", "SELECT 2"]);
    }

    #[test]
    fn doubled_quote_is_escaped_not_a_terminator() {
        assert_eq!(
            split_statements("SELECT 'it''s; fine';"),
            vec!["SELECT 'it''s; fine'"]
        );
    }

    #[test]
    fn comment_only_text_yields_no_statement() {
        assert!(split_statements("-- nothing here\n/* nor here */\n").is_empty());
    }

    #[test]
    fn block_comment_before_a_statement_is_dropped() {
        assert_eq!(split_statements("/* why */ SELECT 1;"), vec!["SELECT 1"]);
    }
}
