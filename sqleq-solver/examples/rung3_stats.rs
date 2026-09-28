// Unless explicitly stated otherwise all files in this repository are licensed under the
// Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/).
// Copyright 2026-Present Datadog, Inc.

//! How often rung 3 (the set solver) applies to rows rung 2 does not prove, and what Z3 says there.
//! A null result from the rung only means something alongside how often it ran.
//!
//! Usage: `cargo run --release --example rung3_stats -- <jobs.jsonl>`

use std::collections::BTreeMap;
use std::io::BufRead;
use std::time::Instant;

use sqleq_solver::alpha::alpha_eq;
use sqleq_solver::ic::Ics;
use sqleq_solver::ir::Input;
use sqleq_solver::normalize::Normalizer;
use sqleq_solver::prove::MAX_TREE_SIZE;
use sqleq_solver::setsolver;
use sqleq_solver::translate::translate_input;

fn main() {
    let path = std::env::args().nth(1).expect("usage: rung3_stats <jobs.jsonl>");
    let mut tally: BTreeMap<&str, usize> = BTreeMap::new();
    let mut slowest = (0u128, String::new());
    for line in std::io::BufReader::new(std::fs::File::open(&path).expect("open")).lines() {
        let line = line.expect("read");
        let mut de = serde_json::Deserializer::from_str(&line);
        de.disable_recursion_limit();
        let row: serde_json::Value = serde::de::Deserialize::deserialize(&mut de).expect("json");
        let ir = &row["ir"];
        if ir.is_null() || ir["queries"][0] == ir["queries"][1] {
            continue;
        }
        let Ok(input) = Input::parse(ir) else { continue };
        let Ok([l, r]) = translate_input(&input) else { continue };
        if l.arity != r.arity {
            continue;
        }
        let ics = Ics::from_schemas(&input.schemas);
        let mut nl = Normalizer::new(l.widths.clone(), MAX_TREE_SIZE).with_ics(ics.clone());
        let mut nr = Normalizer::new(r.widths.clone(), MAX_TREE_SIZE).with_ics(ics);
        let (Ok(tl), Ok(tr)) = (nl.normalize(&l.term), nr.normalize(&r.term)) else { continue };
        if alpha_eq(&tl, &nl.widths, &tr, &nr.widths) {
            *tally.entry("rung 2 proves").or_default() += 1;
            continue;
        }
        let key = match (setsolver::applicable(&tl), setsolver::applicable(&tr)) {
            (true, true) => {
                let t0 = Instant::now();
                let got = setsolver::prove(&tl, &nl.widths, &tr, &nr.widths);
                let ms = t0.elapsed().as_millis();
                if ms > slowest.0 {
                    slowest = (ms, row["name"].as_str().unwrap_or("").to_string());
                }
                if got == Some(true) { "rung 3 applies: proved" } else { "rung 3 applies: not proved" }
            }
            (false, false) => "rung 3 n/a: both sides",
            _ => "rung 3 n/a: one side",
        };
        *tally.entry(key).or_default() += 1;
    }
    for (k, v) in &tally {
        println!("{v:5}  {k}");
    }
    println!("slowest Z3 call: {} ms ({})", slowest.0, slowest.1);
}
