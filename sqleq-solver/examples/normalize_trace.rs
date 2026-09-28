// Unless explicitly stated otherwise all files in this repository are licensed under the
// Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/).
// Copyright 2026-Present Datadog, Inc.

//! Traces normalization of one job row: per side, the tree size after every round, and a
//! summary of which node kinds dominate the term once it stops changing or grows too large.
//!
//! Usage: `cargo run --release --example normalize_trace -- <jobs.jsonl> <pair-name> [<rounds>]`

use std::collections::BTreeMap;
use std::io::BufRead;
use std::time::Instant;

use sqleq_solver::alpha::alpha_eq;
use sqleq_solver::ic::Ics;
use sqleq_solver::ir::Input;
use sqleq_solver::normalize::Normalizer;
use sqleq_solver::translate::translate_input;
use sqleq_solver::uterm::UTerm;

fn kinds(t: &UTerm, depth: usize, out: &mut BTreeMap<String, usize>) {
    let label = match t {
        UTerm::Const(_) => "Const",
        UTerm::Var(_) => "Var",
        UTerm::Table { .. } => "Table",
        UTerm::Pred { .. } => "Pred",
        UTerm::Func { .. } => "Func",
        UTerm::Add(_) => "Add",
        UTerm::Mul(_) => "Mul",
        UTerm::Squash(_) => "Squash",
        UTerm::Neg(_) => "Neg",
        UTerm::Sum { .. } => "Sum",
    };
    *out.entry(format!("{depth:02} {label}")).or_default() += 1;
    if depth >= 6 {
        return;
    }
    match t {
        UTerm::Add(ts) | UTerm::Mul(ts) => ts.iter().for_each(|c| kinds(c, depth + 1, out)),
        UTerm::Squash(c) | UTerm::Neg(c) | UTerm::Sum { body: c, .. } => kinds(c, depth + 1, out),
        _ => {}
    }
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let rounds: usize = args.get(3).and_then(|r| r.parse().ok()).unwrap_or(12);
    let reader = std::io::BufReader::new(std::fs::File::open(&args[1]).expect("open jobs"));
    for line in reader.lines() {
        let line = line.expect("read");
        if !line.contains(&format!("\"{}\"", args[2])) {
            continue;
        }
        let mut de = serde_json::Deserializer::from_str(&line);
        de.disable_recursion_limit();
        let row: serde_json::Value = serde::de::Deserialize::deserialize(&mut de).expect("json");
        if row["name"] != args[2].as_str() {
            continue;
        }
        let input = Input::parse(&row["ir"]).expect("parse");
        let sides = translate_input(&input).expect("translate");
        // Timings of the parts the ladder runs, with and without the integrity constraints.
        for (label, ics) in [("no ICs", Ics::default()), ("ICs", Ics::from_schemas(&input.schemas))] {
            let t0 = Instant::now();
            let mut done = Vec::new();
            for q in &sides {
                let mut n = Normalizer::new(q.widths.clone(), sqleq_solver::prove::MAX_TREE_SIZE).with_ics(ics.clone());
                done.push(n.normalize(&q.term).ok().map(|t| (t, n.widths)));
            }
            let t_norm = t0.elapsed();
            let t1 = Instant::now();
            let eq = match (&done[0], &done[1]) {
                (Some((a, wa)), Some((b, wb))) => Some(alpha_eq(a, wa, b, wb)),
                _ => None,
            };
            println!("[{label}] normalize both: {t_norm:?}, alpha_eq: {:?} -> {eq:?}", t1.elapsed());
            if std::env::var_os("TRACE_PRINT").is_some() {
                for (i, d) in done.iter().enumerate() {
                    if let Some((t, _)) = d {
                        println!("  side {i} normalized: {t:?}");
                    }
                }
            }
        }
        for (i, q) in sides.iter().enumerate() {
            println!("== side {i}: arity {}, translated tree size {}", q.arity, q.term.tree_size(usize::MAX));
            let mut n = Normalizer::new(q.widths.clone(), usize::MAX);
            let mut cur = n.rename_apart(&q.term);
            for r in 0..rounds {
                let next = n.round(&cur);
                let size = next.tree_size(usize::MAX);
                println!("  round {r:2}: {size}");
                let done = next == cur;
                cur = next;
                if done || size > 2_000_000 {
                    break;
                }
            }
            let mut k = BTreeMap::new();
            kinds(&cur, 0, &mut k);
            println!("  node kinds by depth (top 6 levels): {k:?}");
        }
        return;
    }
    eprintln!("no row named {}", args[2]);
}
