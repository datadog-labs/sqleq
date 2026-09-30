// Unless explicitly stated otherwise all files in this repository are licensed under the
// Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/).
// Copyright 2026-Present Datadog, Inc.

//! Ladder check: run the decision ladder (`prove::verify`) over every row of a job file, then join
//! row-by-row against Java's `IrDriver` results for the same file and against the fuzz axis's
//! verdicts, which are the soundness oracle.
//!
//! Usage: `cargo run --release --example phase2_gate -- <jobs.jsonl> <java-results.jsonl>
//! <fuzz-results.jsonl> [<per-row.tsv>]`
//!
//! `<java-results.jsonl>` is `IrDriver`'s output (`{name, verdict, literal?}` per line);
//! `<fuzz-results.jsonl>` is a work dir's `results.jsonl` (`{name, fuzz: {verdict}}` per line).
//! Exits 1 if any non-literal Rust `EQ` lands on a pair the fuzz axis holds a counterexample for, or
//! if any row panics.

use std::collections::{BTreeMap, HashMap};
use std::io::{BufRead, Write};
use std::panic::{self, AssertUnwindSafe};
use std::time::Instant;

use sqleq_solver::prove::{self, Verdict};

fn read_jsonl(path: &str) -> impl Iterator<Item = serde_json::Value> {
    let reader = std::io::BufReader::new(std::fs::File::open(path).unwrap_or_else(|e| panic!("open {path}: {e}")));
    reader.lines().map(|l| l.expect("read line")).filter(|l| !l.trim().is_empty()).map(|line| {
        // Real IR nests past serde_json's default 128-level limit.
        let mut de = serde_json::Deserializer::from_str(&line);
        de.disable_recursion_limit();
        serde::de::Deserialize::deserialize(&mut de).expect("parse json line")
    })
}

fn by_name(path: &str, pick: impl Fn(&serde_json::Value) -> Option<String>) -> HashMap<String, String> {
    read_jsonl(path)
        .filter_map(|r| Some((r.get("name")?.as_str()?.to_string(), pick(&r)?)))
        .collect()
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 4 {
        eprintln!("usage: phase2_gate <jobs.jsonl> <java-results.jsonl> <fuzz-results.jsonl> [<per-row.tsv>]");
        std::process::exit(2);
    }
    let java = by_name(&args[2], |r| {
        let v = r.get("verdict")?.as_str()?;
        Some(if r.get("literal").and_then(|l| l.as_bool()) == Some(true) { format!("{v} literal") } else { v.to_string() })
    });
    let fuzz = by_name(&args[3], |r| Some(r.get("fuzz")?.get("verdict")?.as_str()?.to_string()));
    let mut rows_out = args.get(4).map(|p| std::io::BufWriter::new(std::fs::File::create(p).expect("create per-row output")));

    let mut rust_counts: BTreeMap<String, usize> = BTreeMap::new();
    let mut cross: BTreeMap<(String, String), usize> = BTreeMap::new();
    let mut gained: Vec<String> = Vec::new();
    let mut lost: Vec<String> = Vec::new();
    let mut refuted: Vec<String> = Vec::new();
    let mut panics: Vec<String> = Vec::new();
    let mut slowest: Vec<(u128, String)> = Vec::new();
    let started = Instant::now();

    for row in read_jsonl(&args[1]) {
        let name = row.get("name").and_then(|v| v.as_str()).unwrap_or("<unnamed>").to_string();
        let ir = match row.get("ir") {
            Some(ir) if !ir.is_null() => ir,
            _ => continue,
        };
        let t0 = Instant::now();
        let verdict = match panic::catch_unwind(AssertUnwindSafe(|| prove::verify(ir))) {
            Ok(v) => v,
            Err(_) => {
                panics.push(name.clone());
                continue;
            }
        };
        let micros = t0.elapsed().as_micros();
        slowest.push((micros, name.clone()));

        // Bucket refusals and not-proved reasons by kind only, so the table stays readable.
        let rust = match &verdict {
            Verdict::Refused(_) => "NOTRANS".to_string(),
            other => other.to_string(),
        };
        *rust_counts.entry(rust.clone()).or_default() += 1;
        let java_v = java.get(&name).cloned().unwrap_or_else(|| "<absent>".to_string());
        *cross.entry((java_v.clone(), rust.clone())).or_default() += 1;
        if let Some(w) = rows_out.as_mut() {
            // Flushed per row, so a run killed on a slow row still names it.
            writeln!(w, "{name}\t{verdict}\t{}", micros / 1000).expect("write per-row output");
            w.flush().expect("flush per-row output");
        }

        let rust_proved = verdict == Verdict::Eq { literal: false };
        let java_proved = java_v == "EQ";
        if rust_proved && !java_proved {
            gained.push(format!("{name} (java {java_v}, fuzz {})", fuzz.get(&name).map_or("<absent>", String::as_str)));
        }
        if java_proved && !matches!(verdict, Verdict::Eq { .. }) {
            lost.push(format!("{name} (rust {verdict})"));
        }
        if rust_proved && fuzz.get(&name).map(String::as_str) == Some("counterexample") {
            refuted.push(name.clone());
        }
    }
    if let Some(mut w) = rows_out {
        w.flush().expect("flush per-row output");
    }

    println!("=== ladder check: {} ({:.1}s) ===", args[1], started.elapsed().as_secs_f64());
    println!("-- rust verdicts --");
    for (v, c) in &rust_counts {
        println!("  {c:5}  {v}");
    }
    println!("-- java x rust --");
    for ((j, r), c) in &cross {
        println!("  {c:5}  java={j:<14} rust={r}");
    }
    println!("-- java non-literal EQ not proved by rust: {} --", lost.len());
    for l in &lost {
        println!("  {l}");
    }
    println!("-- rust non-literal EQ java lacks: {} --", gained.len());
    for g in &gained {
        println!("  {g}");
    }
    slowest.sort();
    println!("-- slowest rows --");
    for (us, n) in slowest.iter().rev().take(5) {
        println!("  {:>10.3} ms  {n}", *us as f64 / 1000.0);
    }
    println!();
    println!("rust EQ on a fuzz counterexample: {}", refuted.len());
    for r in &refuted {
        println!("  REFUTED: {r}");
    }
    println!("panics: {}", panics.len());
    for p in &panics {
        println!("  PANIC: {p}");
    }
    if refuted.is_empty() && panics.is_empty() {
        println!("GATE (soundness): PASS");
    } else {
        println!("GATE (soundness): FAIL");
        std::process::exit(1);
    }
}
