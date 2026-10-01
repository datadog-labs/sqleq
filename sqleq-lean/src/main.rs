// Unless explicitly stated otherwise all files in this repository are licensed under the
// Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/).
// Copyright 2026-Present Datadog, Inc.

use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Duration;

use sqleq_lean::{check, run::Lean, Case};

const USAGE: &str = "\
usage: sqleq-lean [options] <pair.sql | dir>...
       sqleq-lean [options] --csv <corpus.csv> [--names <file>]

Checks INSERT ... VALUES vs INSERT ... SELECT * FROM unnest(..) pairs in Lean.

options:
  --json <file>     write {case: {verdict, reason, shape, flipped, ms}} here (default: stdout)
  --batch <n>       pairs per Lean file (default 50)
  --jobs <n>        Lean processes at once (default 4)
  --timeout <secs>  per Lean file (default 600)
  --keep <dir>      keep the generated .lean files in <dir>
  --replay-plan <file>  also write, for each proved or no-witness pair, what
                    tools/lean_replay.py needs to re-run it on Postgres
  --full-names      key a pair file's record by its path, not its file name

environment: LAKE (default `lake` on PATH), SQLEQ_LEAN_DIR (default the repository's lean/)";

fn sql_files(p: &Path, out: &mut Vec<PathBuf>) -> std::io::Result<()> {
    if p.is_dir() {
        let mut entries: Vec<_> = std::fs::read_dir(p)?.collect::<Result<_, _>>()?;
        entries.sort_by_key(|e| e.path());
        for e in entries {
            sql_files(&e.path(), out)?;
        }
    } else if p.extension().is_some_and(|x| x == "sql") {
        out.push(p.to_path_buf());
    }
    Ok(())
}

fn main() -> ExitCode {
    let mut args = std::env::args().skip(1);
    let (mut json, mut csv, mut names, mut keep, mut plan) = (None, None, None, None, None);
    let (mut batch, mut jobs, mut timeout) = (50usize, 4usize, 600u64);
    let mut full_names = false;
    let mut paths = Vec::new();
    while let Some(a) = args.next() {
        let mut val = |flag: &str| args.next().ok_or_else(|| format!("{flag} needs a value"));
        let r: Result<(), String> = (|| {
            match a.as_str() {
                "--json" => json = Some(PathBuf::from(val("--json")?)),
                "--csv" => csv = Some(PathBuf::from(val("--csv")?)),
                "--names" => names = Some(PathBuf::from(val("--names")?)),
                "--keep" => keep = Some(PathBuf::from(val("--keep")?)),
                "--replay-plan" => plan = Some(PathBuf::from(val("--replay-plan")?)),
                "--batch" => batch = val("--batch")?.parse().map_err(|e| format!("--batch: {e}"))?,
                "--jobs" => jobs = val("--jobs")?.parse().map_err(|e| format!("--jobs: {e}"))?,
                "--timeout" => timeout = val("--timeout")?.parse().map_err(|e| format!("--timeout: {e}"))?,
                "--full-names" => full_names = true,
                "-h" | "--help" => return Err(String::new()),
                s if s.starts_with('-') => return Err(format!("unknown option {s}")),
                _ => paths.push(PathBuf::from(&a)),
            }
            Ok(())
        })();
        if let Err(e) = r {
            if !e.is_empty() {
                eprintln!("sqleq-lean: {e}");
            }
            eprintln!("{USAGE}");
            return ExitCode::from(2);
        }
    }

    let mut cases = Vec::new();
    if let Some(csv) = &csv {
        let only: Option<std::collections::HashSet<String>> = match &names {
            None => None,
            Some(p) => match std::fs::read_to_string(p) {
                Ok(t) => Some(t.lines().map(|l| l.trim().to_string()).filter(|l| !l.is_empty()).collect()),
                Err(e) => {
                    eprintln!("sqleq-lean: {}: {e}", p.display());
                    return ExitCode::from(2);
                }
            },
        };
        let rows = match sqleq_frontend::corpus::read(csv) {
            Ok(r) => r,
            Err(e) => {
                eprintln!("sqleq-lean: {}: {e}", csv.display());
                return ExitCode::from(2);
            }
        };
        for row in rows {
            let name = row.name();
            if only.as_ref().is_some_and(|o| !o.contains(&name)) {
                continue;
            }
            cases.push(Case::from_row(name, &row.a, &row.b, row.ddl.as_deref()));
        }
    } else {
        let mut files = Vec::new();
        for p in &paths {
            if let Err(e) = sql_files(p, &mut files) {
                eprintln!("sqleq-lean: {}: {e}", p.display());
                return ExitCode::from(2);
            }
        }
        for f in files {
            let name = if full_names {
                f.to_string_lossy().to_string()
            } else {
                f.file_name().unwrap().to_string_lossy().to_string()
            };
            match std::fs::read_to_string(&f) {
                Ok(t) => cases.push(Case::from_file(name, &t)),
                Err(e) => {
                    eprintln!("sqleq-lean: {}: {e}", f.display());
                    return ExitCode::from(2);
                }
            }
        }
    }
    if cases.is_empty() {
        eprintln!("{USAGE}");
        return ExitCode::from(2);
    }

    let lean = Lean::from_env(Duration::from_secs(timeout));
    let records = match check(&cases, &lean, batch, jobs, keep.as_deref(), plan.is_some()) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("sqleq-lean: {e}");
            return ExitCode::from(1);
        }
    };
    let mut plans = serde_json::Map::new();
    let obj: serde_json::Map<String, serde_json::Value> = records
        .into_iter()
        .map(|(n, mut r)| {
            if let Some(p) = r.as_object_mut().and_then(|o| o.remove("replay")) {
                let mut p = p;
                p["verdict"] = r["verdict"].clone();
                if let Some(reason) = r.get("reason") {
                    p["reason"] = reason.clone();
                }
                plans.insert(n.clone(), p);
            }
            (n, r)
        })
        .collect();
    if let Some(p) = &plan {
        let text = serde_json::to_string(&serde_json::Value::Object(plans)).unwrap();
        if let Err(e) = std::fs::write(p, text + "\n") {
            eprintln!("sqleq-lean: {}: {e}", p.display());
            return ExitCode::from(1);
        }
    }
    let text = serde_json::to_string_pretty(&serde_json::Value::Object(obj)).unwrap();
    match &json {
        Some(p) => {
            if let Err(e) = std::fs::write(p, text + "\n") {
                eprintln!("sqleq-lean: {}: {e}", p.display());
                return ExitCode::from(1);
            }
        }
        None => println!("{text}"),
    }
    ExitCode::SUCCESS
}
