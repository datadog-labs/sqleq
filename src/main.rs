// Unless explicitly stated otherwise all files in this repository are licensed under the
// Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/).
// Copyright 2026-Present Datadog, Inc.

//! CLI: `sqleq-frontend [--infer|--infer-seeded] <input.sql> [out.json]` — lower a SQL pair to the
//! prover's `Input` JSON.
//!
//! This is the binary `sqleq-check` runs for its `frontend` axis, and the one it hands the provers'
//! input from. To check pairs end to end, run `sqleq-check`; this is for driving the lowering
//! alone.
//!
//! Writes the JSON to `out.json` (default: the input path with a `.fe.json` extension) and prints
//! that path on success. On any lowering error it prints the reason to stderr and exits non-zero —
//! the frontend never emits partial/best-effort IR. When the two sides of a refused pair normalize
//! to one query, the line [`sqleq_frontend::REFLEXIVE_NOTE`] comes first, so the refusal is still
//! the last line a caller reads.
//!
//! By default the schema is read from the input's `CREATE TABLE`s. The two flags turn type inference
//! on instead; see [`sqleq_frontend::CatalogSource`] for what each decides, and note that they also
//! enable the cast rules and the `DECLARE` synthesis, which are not separable from it.
//!
//! `--ddl <file>` reads the schema from raw Postgres DDL instead, ignoring any `CREATE TABLE`s in
//! the input. It composes with the inference flags.
//!
//! `--csv <corpus.csv> -o <dir>` is the batch form: every row of the corpus is lowered directly,
//! reading its DDL out of the row, and no `.sql` intermediate is written. `--report <report.json>`
//! adds every row's outcome and the refusal histogram; `--limit N` stops after the first N rows.
//! See [`sqleq_frontend::corpus`].
//!
//! `--sqlsolver --ir <input.json>` is the only mode whose input is an `Input` rather than SQL: it
//! packages an *already lowered* plan as one job for a SQLSolver driver — `sqleq-solver`, or the
//! JVM fork's `IrDriver` — deriving the DDL from the plan's own `schemas`. The job is named by
//! `--name <id>` (default: the file stem) and written to `-o <job.jsonl>` (default: stdout). It
//! lowers nothing — deliberately, so that `sqleq-check` can hand the SQLSolver axis the very bytes
//! it handed the QED prover, with no second lowering between the two. See
//! [`sqleq_frontend::sqlsolver::ir_job_from_input`].

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use sqleq_frontend::{corpus, CatalogSource};

const USAGE: &str = "usage: sqleq-frontend [--infer|--infer-seeded] [--ddl <schema.sql>] <input.sql> [out.json]
       sqleq-frontend [--infer|--infer-seeded] --csv <corpus.csv> -o <dir> [--report <report.json>] [--limit N]
       sqleq-frontend --sqlsolver --ir <input.json> [--name <id>] [-o <job.jsonl>]

Lowers a SQL pair to prover input. To check pairs end to end, run sqleq-check.";

fn main() -> ExitCode {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    // Before anything is parsed as a path, or `--help` is read as an input file and reported as
    // missing -- the worst answer of the three this binary can give to someone new to it.
    if args.iter().any(|a| a == "--help" || a == "-h") {
        println!("{USAGE}");
        return ExitCode::SUCCESS;
    }
    let sqlsolver = args.iter().any(|a| a == "--sqlsolver");
    args.retain(|a| a != "--sqlsolver");
    // Only meaningful with `--sqlsolver`; see [`sqleq_frontend::sqlsolver::ir_job_from_input`].
    let ir = args.iter().any(|a| a == "--ir");
    args.retain(|a| a != "--ir");
    let mut source = CatalogSource::Declared;
    for (flag, mode) in [
        ("--infer", CatalogSource::Inferred),
        ("--infer-seeded", CatalogSource::InferredSeeded),
    ] {
        if args.iter().any(|a| a == flag) {
            source = mode;
            args.retain(|a| a != flag);
        }
    }
    let mut opt = |name: &str| match args.iter().position(|a| a == name) {
        Some(i) if i + 1 < args.len() => {
            let v = args.remove(i + 1);
            args.remove(i);
            Some(v)
        }
        Some(_) => {
            eprintln!("{name} needs a value");
            std::process::exit(2);
        }
        None => None,
    };
    let ddl_path = opt("--ddl");
    let csv_path = opt("--csv");
    let outdir = opt("-o").or_else(|| opt("--outdir"));
    let report = opt("--report");
    let limit = opt("--limit").and_then(|s| s.parse::<usize>().ok());
    let name_override = opt("--name");

    if ir && !sqlsolver {
        eprintln!("--ir is only meaningful with --sqlsolver");
        return ExitCode::from(2);
    }
    // The one SQLSolver-axis mode: package one already-lowered plan. It has no corpus form, because
    // the plan it packages is the one a caller has already handed the QED prover.
    if sqlsolver {
        if !ir || csv_path.is_some() || args.is_empty() {
            eprintln!("--sqlsolver needs --ir <input.json>");
            return ExitCode::from(2);
        }
        return run_ir_job(Path::new(&args[0]), name_override.as_deref(), outdir.as_deref());
    }

    if let Some(csv) = csv_path {
        return run_csv(Path::new(&csv), outdir.as_deref(), report.as_deref(), limit, source);
    }

    if args.is_empty() {
        eprintln!("{USAGE}");
        return ExitCode::from(2);
    }
    let path = &args[0];
    let out = args
        .get(1)
        .cloned()
        .unwrap_or_else(|| format!("{}.fe.json", path.trim_end_matches(".sql")));

    let src = match std::fs::read_to_string(path) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("cannot read {path}: {e}");
            return ExitCode::FAILURE;
        }
    };

    let ddl = match ddl_path.as_deref().map(std::fs::read_to_string) {
        Some(Ok(d)) => Some(d),
        Some(Err(e)) => {
            eprintln!("cannot read {}: {e}", ddl_path.unwrap());
            return ExitCode::FAILURE;
        }
        None => None,
    };

    let lowered = match &ddl {
        Some(d) => sqleq_frontend::lower_with_ddl(&src, d, source),
        None => sqleq_frontend::lower_with(&src, source),
    };
    match lowered {
        Ok(input) => {
            let json = serde_json::to_string_pretty(&input).expect("serialize");
            if let Err(e) = std::fs::write(&out, json) {
                eprintln!("cannot write {out}: {e}");
                return ExitCode::FAILURE;
            }
            println!("{out}");
            ExitCode::SUCCESS
        }
        Err(e) => {
            // The corpus mode gives such a row a status of its own; a single pair says it here,
            // ahead of the refusal, so the refusal is still the line a caller classifies.
            if sqleq_frontend::reflexive(&src) {
                eprintln!("{}", sqleq_frontend::REFLEXIVE_NOTE);
            }
            eprintln!("{e}");
            ExitCode::FAILURE
        }
    }
}

/// Package one already-lowered `Input` JSON as a bridge job for the SQLSolver axis.
///
/// The only mode that reads an `Input` instead of SQL, and the asymmetry is the point: the caller
/// has already lowered this case and fed the JSON to our own prover, so lowering it again here
/// would put a second frontend pass between the two axes and let them drift. The SQLSolver driver
/// (`sqleq-solver`, or the JVM fork's `IrDriver`) gets the same bytes, plus the MySQL DDL those
/// bytes imply.
///
/// A plan that cannot be bridged is refused with a reason on stderr and a non-zero exit, not
/// written as an `ir: null` job: a caller that named one file wants to be told.
fn run_ir_job(path: &Path, name: Option<&str>, out: Option<&str>) -> ExitCode {
    let src = match std::fs::read_to_string(path) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("cannot read {}: {e}", path.display());
            return ExitCode::FAILURE;
        }
    };
    let ir: serde_json::Value = match serde_json::from_str(&src) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("{} is not an Input JSON: {e}", path.display());
            return ExitCode::from(2);
        }
    };
    // A plan file with no `queries` is not an `Input`, and the bridge's tier 0 would read the
    // absence as "two identical trees" -- an EQ out of thin air. Cheaper to refuse here.
    if ir["queries"].as_array().is_none_or(|q| q.len() != 2) {
        eprintln!("{}: expected an Input with exactly two `queries`", path.display());
        return ExitCode::from(2);
    }
    let name = name
        .map(str::to_string)
        .or_else(|| path.file_stem().map(|s| s.to_string_lossy().into_owned()))
        .unwrap_or_else(|| "case".to_string());
    let job = sqleq_frontend::sqlsolver::ir_job_from_input(name, ir);
    if let Some(r) = &job.refusal {
        eprintln!("{r}");
        return ExitCode::FAILURE;
    }
    let line = format!("{}\n", job.to_json());
    match out {
        Some(out) => {
            if let Err(e) = std::fs::write(out, line) {
                eprintln!("cannot write {out}: {e}");
                return ExitCode::FAILURE;
            }
            println!("{out}");
        }
        None => print!("{line}"),
    }
    ExitCode::SUCCESS
}

/// Lower a whole corpus CSV. Exits 0 whatever the emit rate: refusals are the measurement here, not
/// a failure, and the report is where they are counted.
fn run_csv(
    csv: &Path,
    outdir: Option<&str>,
    report: Option<&str>,
    limit: Option<usize>,
    source: CatalogSource,
) -> ExitCode {
    let Some(outdir) = outdir else {
        eprintln!("--csv needs -o <dir>");
        return ExitCode::from(2);
    };
    let outdir = PathBuf::from(outdir);
    if let Err(e) = std::fs::create_dir_all(&outdir) {
        eprintln!("cannot create {}: {e}", outdir.display());
        return ExitCode::FAILURE;
    }
    let mut rows = match corpus::read(csv) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("{e}");
            return ExitCode::FAILURE;
        }
    };
    if let Some(n) = limit {
        rows.truncate(n);
    }

    let mut emitted = 0usize;
    let mut reflexive = 0usize;
    let mut detail = Vec::with_capacity(rows.len());
    let mut kinds: std::collections::BTreeMap<&str, usize> = Default::default();
    let mut reasons: std::collections::BTreeMap<String, usize> = Default::default();
    for row in &rows {
        let outcome = corpus::lower(row, source);
        // Only on the refusal path: a row that lowers is already reported by its IR, and asking the
        // question of the rows that do would be work for an answer nothing reads.
        let refl = outcome.is_err() && corpus::reflexive(row);
        reflexive += usize::from(refl);
        detail.push(corpus::record(row, &outcome, refl));
        match &outcome {
            Ok(input) => {
                let path = outdir.join(format!("{}.json", row.name()));
                let json = serde_json::to_string_pretty(input).expect("serialize");
                if let Err(e) = std::fs::write(&path, json) {
                    eprintln!("cannot write {}: {e}", path.display());
                    return ExitCode::FAILURE;
                }
                emitted += 1;
            }
            Err(e) => {
                *kinds.entry(corpus::kind(e)).or_default() += 1;
                *reasons.entry(generalize(&e.to_string())).or_default() += 1;
            }
        }
    }

    println!("rows processed : {}", rows.len());
    println!("cases emitted  : {emitted}  -> {}", outdir.display());
    println!("refused        : {}", rows.len() - emitted);
    // Counted inside `refused`, not beside it: the histogram below is still the first-failure
    // histogram it always was, and these rows still have the construct that stopped them. What
    // changed is that they no longer need it lowered.
    println!("  of which settled by reflexivity: {reflexive}");
    for (k, n) in &kinds {
        println!("  {n:>5}  {k}");
    }
    println!("\ntop refusal reasons:");
    let mut top: Vec<_> = reasons.iter().collect();
    top.sort_by(|a, b| b.1.cmp(a.1).then(a.0.cmp(b.0)));
    for (reason, n) in top.iter().take(25) {
        println!("  {n:>5}  {reason}");
    }

    if let Some(path) = report {
        let out = serde_json::json!({
            "total": rows.len(),
            "emitted": emitted,
            "reflexive": reflexive,
            "kinds": kinds,
            "reasons": reasons,
            "detail": detail,
        });
        if let Err(e) = std::fs::write(path, serde_json::to_string_pretty(&out).expect("serialize"))
        {
            eprintln!("cannot write {path}: {e}");
            return ExitCode::FAILURE;
        }
        println!("\nwrote {path}");
    }
    ExitCode::SUCCESS
}

/// Collapse the identifiers out of a refusal message so it can be counted as a bucket: `unknown
/// table "items"` and `unknown table "vendors"` are one reason, not two.
fn generalize(msg: &str) -> String {
    let mut out = String::with_capacity(msg.len());
    let mut in_quotes = false;
    let mut last_digit = false;
    for c in msg.chars() {
        match c {
            '"' | '\'' | '`' => {
                if !in_quotes {
                    out.push('X');
                }
                in_quotes = !in_quotes;
                last_digit = false;
            }
            _ if in_quotes => {}
            '0'..='9' => {
                if !last_digit {
                    out.push('N');
                }
                last_digit = true;
            }
            _ => {
                out.push(c);
                last_digit = false;
            }
        }
    }
    out
}
