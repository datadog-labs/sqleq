// Unless explicitly stated otherwise all files in this repository are licensed under the
// Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/).
// Copyright 2026-Present Datadog, Inc.

//! Translation check: every row of a job file whose `ir` is non-null must translate without
//! panicking, and the resulting refusal set is reported so it can be compared against `IrToRel`'s
//! own taxonomy on the same rows.
//!
//! Usage: `cargo run --example phase1_gate -- <path-to-ir.jobs.jsonl> [<per-row.tsv>]`
//!
//! The optional second argument writes one `name<TAB>stage<TAB>outcome` line per attempted row
//! (`stage` is `parse` or `translate`, `outcome` is `ok` or the refusal/panic text), so the outcome
//! can be joined row-by-row against `IrDriver`'s results for the same file.

use std::collections::BTreeMap;
use std::io::{BufRead, Write};
use std::panic::{self, AssertUnwindSafe};

use sqleq_solver::{ir, translate};

const SIZE_CAP: usize = 1_000_000_000;

fn main() {
    let path = std::env::args().nth(1).expect("usage: phase1_gate <jobs.jsonl> [<per-row.tsv>]");
    let mut rows_out = std::env::args()
        .nth(2)
        .map(|p| std::io::BufWriter::new(std::fs::File::create(p).expect("create per-row output")));
    let mut record = |name: &str, stage: &str, outcome: &str| {
        if let Some(w) = rows_out.as_mut() {
            writeln!(w, "{name}\t{stage}\t{outcome}").expect("write per-row output");
        }
    };
    let file = std::fs::File::open(&path).expect("open jobs file");
    let reader = std::io::BufReader::new(file);

    let mut total = 0usize;
    let mut null_ir = 0usize;
    let mut parse_err: BTreeMap<String, usize> = BTreeMap::new();
    let mut parse_panic: Vec<String> = Vec::new();
    let mut translate_ok = 0usize;
    let mut translate_err: BTreeMap<String, usize> = BTreeMap::new();
    let mut translate_panic: Vec<String> = Vec::new();
    let mut sizes: Vec<(usize, String)> = Vec::new();

    for line in reader.lines() {
        let line = line.expect("read line");
        if line.trim().is_empty() {
            continue;
        }
        total += 1;
        // The IR's expression trees nest well past serde_json's default 128-level recursion
        // limit (long AND/OR/CAST chains) -- this is real, not adversarial, so lift the limit
        // rather than the input.
        let mut deserializer = serde_json::Deserializer::from_str(&line);
        deserializer.disable_recursion_limit();
        let row: serde_json::Value =
            serde::de::Deserialize::deserialize(&mut deserializer).expect("parse job row json");
        let name = row.get("name").and_then(|v| v.as_str()).unwrap_or("<unnamed>").to_string();
        let ir_value = row.get("ir").cloned().unwrap_or(serde_json::Value::Null);
        if ir_value.is_null() {
            null_ir += 1;
            continue;
        }

        eprint!("{name}\r");
        let _ = std::io::stderr().flush();

        let parsed = panic::catch_unwind(AssertUnwindSafe(|| ir::Input::parse(&ir_value)));
        let input = match parsed {
            Ok(Ok(input)) => input,
            Ok(Err(e)) => {
                let reason = format!("{e}");
                record(&name, "parse", &reason);
                *parse_err.entry(reason).or_default() += 1;
                continue;
            }
            Err(payload) => {
                let msg = panic_message(&payload);
                record(&name, "parse", &format!("PANIC {msg}"));
                parse_panic.push(format!("{name}: {msg}"));
                continue;
            }
        };

        let translated = panic::catch_unwind(AssertUnwindSafe(|| translate::translate_input(&input)));
        match translated {
            Ok(Ok(queries)) => {
                let size = queries.iter().map(|q| q.term.tree_size(SIZE_CAP)).max().unwrap_or(0);
                sizes.push((size, name.clone()));
                record(&name, "translate", "ok");
                translate_ok += 1;
            }
            Ok(Err(e)) => {
                let reason = format!("{e}");
                record(&name, "translate", &reason);
                *translate_err.entry(reason).or_default() += 1;
            }
            Err(payload) => {
                let msg = panic_message(&payload);
                record(&name, "translate", &format!("PANIC {msg}"));
                translate_panic.push(format!("{name}: {msg}"));
            }
        }
    }

    // Flushed by hand: the FAIL path below leaves via `process::exit`, which skips `Drop`.
    if let Some(mut w) = rows_out {
        w.flush().expect("flush per-row output");
    }

    let attempted = total - null_ir;
    println!("=== translation check: {path} ===");
    println!("total rows:              {total}");
    println!("null-ir (frontend refusal, not attempted): {null_ir}");
    println!("attempted (ir.rs + translate.rs):          {attempted}");
    println!();
    println!("-- ir.rs parse stage --");
    println!("parse ok:    {}", attempted - parse_err.values().sum::<usize>() - parse_panic.len());
    println!("parse err:   {}", parse_err.values().sum::<usize>());
    println!("parse panic: {}", parse_panic.len());
    for (reason, count) in &parse_err {
        println!("  {count:5}  {reason}");
    }
    for p in &parse_panic {
        println!("  PANIC: {p}");
    }
    println!();
    println!("-- translate.rs stage --");
    println!("translate ok:    {translate_ok}");
    println!("translate err:   {}", translate_err.values().sum::<usize>());
    println!("translate panic: {}", translate_panic.len());
    for (reason, count) in &translate_err {
        println!("  {count:5}  {reason}");
    }
    for p in &translate_panic {
        println!("  PANIC: {p}");
    }
    println!();
    // Tree size (shared subterms counted per occurrence) is what a non-memoizing pass over the term
    // pays, so its tail decides whether later phases need a size cap.
    sizes.sort();
    if !sizes.is_empty() {
        let pct = |p: usize| sizes[(sizes.len() - 1) * p / 100].0;
        println!("-- translated term tree size (larger side, capped at {SIZE_CAP}) --");
        println!("p50 {}  p90 {}  p99 {}  max {}", pct(50), pct(90), pct(99), sizes[sizes.len() - 1].0);
        for threshold in [10_000, 100_000, 1_000_000, 10_000_000] {
            println!("  > {threshold:>10}: {}", sizes.iter().filter(|(s, _)| *s > threshold).count());
        }
        for (s, n) in sizes.iter().rev().take(8) {
            println!("  {s:>12}  {n}");
        }
        println!();
    }
    let total_panics = parse_panic.len() + translate_panic.len();
    if total_panics == 0 {
        println!("GATE: PASS -- 0 panics across {attempted} attempted rows");
    } else {
        println!("GATE: FAIL -- {total_panics} panics across {attempted} attempted rows");
        std::process::exit(1);
    }
}

fn panic_message(payload: &(dyn std::any::Any + Send)) -> String {
    if let Some(s) = payload.downcast_ref::<&str>() {
        s.to_string()
    } else if let Some(s) = payload.downcast_ref::<String>() {
        s.clone()
    } else {
        "<non-string panic payload>".to_string()
    }
}
