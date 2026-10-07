// Unless explicitly stated otherwise all files in this repository are licensed under the
// Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/).
// Copyright 2026-Present Datadog, Inc.

//! Semantic check of rung 2 on real rows: normalization must not change what a term means, and a
//! pair the ladder proves must agree on concrete data. For every translatable row of a job file,
//! each side is evaluated ([`sqleq_solver::eval`]) on a few small random databases, before and
//! after normalization, and for proved pairs side A against side B. The comparison is the total
//! result count, `Σ_out term` -- coarser than the result bag, but it moves whenever a rewrite drops
//! a filter, a multiplicity or a row.
//!
//! Usage: `cargo run --release --example normalize_check -- <jobs.jsonl> [<trials>]`
//! Exits 1 on any mismatch. Trials the evaluator cannot run (a sum it would have to enumerate over
//! too large a universe) are counted as skipped, never as passes.

use std::collections::{BTreeSet, HashMap};
use std::io::BufRead;
use std::rc::Rc;

use sqleq_solver::eval::{Db, Env};
use sqleq_solver::ic::Ics;
use sqleq_solver::ir::{Input, Type};
use sqleq_solver::normalize::Normalizer;
use sqleq_solver::prove::{self, Verdict};
use sqleq_solver::translate::{translate_input, Query, OUT_VAR_ID};
use sqleq_solver::uterm::{UConst, UTerm, UVar};

const MAX_TREE: usize = 50_000;

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
}

fn constants(t: &UTerm, out: &mut BTreeSet<String>, vals: &mut Vec<UConst>) {
    match t {
        UTerm::Const(c) => {
            if out.insert(format!("{c:?}")) {
                vals.push(c.clone());
            }
        }
        UTerm::Var(_) | UTerm::Table { .. } => {}
        UTerm::Pred { args, .. } | UTerm::Func { args, .. } => args.iter().for_each(|a| constants(a, out, vals)),
        UTerm::Add(ts) | UTerm::Mul(ts) => ts.iter().for_each(|c| constants(c, out, vals)),
        UTerm::Squash(c) | UTerm::Neg(c) | UTerm::Sum { body: c, .. } => constants(c, out, vals),
    }
}

/// Whether `v` may sit in a column of type `ty`: a number in a numeric or temporal one (a temporal
/// type is an integer in its own unit), a string in a VARCHAR one, anything in an opaque one, and
/// NULL anywhere. The IR is typed, so no query compares a string with a number; a row that put one
/// in an INTEGER column would test an order comparison on a pair no database can hold.
fn fits(v: &UConst, ty: &Type) -> bool {
    match (v, ty) {
        (UConst::Null, _) | (_, Type::Boolean | Type::Varbinary | Type::Interval) => true,
        (UConst::Str(_), ty) => *ty == Type::Varchar,
        (_, ty) => *ty != Type::Varchar,
    }
}

/// `Σ_out term`: the total number of result rows, with multiplicity.
fn total(term: &UTerm, db: &Db) -> Result<i64, String> {
    let closed = UTerm::Sum { vars: vec![UVar::Base(OUT_VAR_ID)], body: Rc::new(term.clone()) };
    db.count(&closed, &mut Env::new())
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let trials: usize = args.get(2).and_then(|t| t.parse().ok()).unwrap_or(5);
    let reader = std::io::BufReader::new(std::fs::File::open(&args[1]).expect("open jobs"));
    let (mut rows, mut compared, mut skipped, mut proved_checked) = (0usize, 0usize, 0usize, 0usize);
    let mut mismatches: Vec<String> = Vec::new();

    for line in reader.lines() {
        let line = line.expect("read");
        let mut de = serde_json::Deserializer::from_str(&line);
        de.disable_recursion_limit();
        let row: serde_json::Value = serde::de::Deserialize::deserialize(&mut de).expect("json");
        let name = row["name"].as_str().unwrap_or("").to_string();
        let ir = &row["ir"];
        if ir.is_null() || ir["queries"][0] == ir["queries"][1] {
            continue;
        }
        let Ok(input) = Input::parse(ir) else { continue };
        let Ok(sides) = translate_input(&input) else { continue };
        if sides.iter().any(|q: &Query| q.term.tree_size(MAX_TREE + 1) > MAX_TREE) {
            continue;
        }
        let ics = Ics::from_schemas(&input.schemas);
        // Each side normalized without and with the integrity constraints. Databases below always
        // satisfy the constraints: those are the only ones a query runs on, and the only ones the
        // constrained rewrites promise anything about.
        let normalized: Vec<Vec<(UTerm, HashMap<u32, usize>)>> = sides
            .iter()
            .map(|q| {
                [Ics::default(), ics.clone()]
                    .into_iter()
                    .filter_map(|c| {
                        let mut n = Normalizer::new(q.widths.clone(), prove::MAX_TREE_SIZE).with_ics(c);
                        n.normalize(&q.term).ok().map(|t| (t, n.widths))
                    })
                    .collect()
            })
            .collect();
        let proved = prove::verify(ir) == Verdict::Eq { literal: false };
        rows += 1;
        if rows.is_multiple_of(50) {
            eprintln!("{rows} rows ({name}): compared {compared}, skipped {skipped}, mismatches {}", mismatches.len());
        }

        let (mut seen, mut universe) = (BTreeSet::new(), vec![UConst::Null, UConst::Int(0), UConst::Int(1), UConst::Int(2), UConst::Str("a".into())]);
        for u in &universe {
            seen.insert(format!("{u:?}"));
        }
        for q in &sides {
            constants(&q.term, &mut seen, &mut universe);
        }
        let mut rng = Rng(name.bytes().fold(0x9e3779b97f4a7c15u64, |h, b| h.rotate_left(5) ^ b as u64) | 1);

        for trial in 0..trials {
            let tables: HashMap<String, Vec<Vec<UConst>>> = input
                .schemas
                .iter()
                .map(|s| {
                    let not_null = ics.not_null.get(&s.name);
                    let keys = ics.keys.get(&s.name).cloned().unwrap_or_default();
                    let mut rows: Vec<Vec<UConst>> = Vec::new();
                    for _ in 0..rng.below(4) {
                        let row: Vec<UConst> = (0..s.types.len())
                            .map(|c| loop {
                                let v = universe[rng.below(universe.len())].clone();
                                let nullable = !not_null.is_some_and(|nn| nn.contains(&(c as u32)));
                                if fits(&v, &s.types[c]) && (v != UConst::Null || nullable) {
                                    break v;
                                }
                            })
                            .collect();
                        // A row that repeats an existing row's key would break the key: leave it out.
                        let clashes = keys.iter().any(|k| rows.iter().any(|r| k.iter().all(|&c| r[c as usize] == row[c as usize])));
                        if !clashes {
                            rows.push(row);
                        }
                    }
                    (s.name.clone(), rows)
                })
                .collect();
            let mut totals = Vec::new();
            for (i, q) in sides.iter().enumerate() {
                let db = Db::new(tables.clone(), universe.clone(), q.widths.clone());
                let before = total(&q.term, &db);
                for (k, (t, w)) in normalized[i].iter().enumerate() {
                    match (&before, total(t, &Db::new(tables.clone(), universe.clone(), w.clone()))) {
                        (Ok(b), Ok(a)) => {
                            compared += 1;
                            if a != *b {
                                let how = if k == 0 { "normalized" } else { "normalized with ICs" };
                                mismatches.push(format!("{name} side {i} trial {trial}: translated {b}, {how} {a}"));
                            }
                        }
                        _ => skipped += 1,
                    }
                }
                totals.push(before);
            }
            if proved {
                if let [Ok(a), Ok(b)] = totals.as_slice() {
                    proved_checked += 1;
                    if a != b {
                        mismatches.push(format!("{name} PROVED EQ but trial {trial} totals differ: {a} vs {b}"));
                    }
                }
            }
        }
    }

    println!("rows checked: {rows}  side-trials compared: {compared}  skipped: {skipped}  proved-pair trials: {proved_checked}");
    println!("mismatches: {}", mismatches.len());
    for m in mismatches.iter().take(40) {
        println!("  {m}");
    }
    if !mismatches.is_empty() {
        std::process::exit(1);
    }
}
