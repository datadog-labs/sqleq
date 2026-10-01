// Unless explicitly stated otherwise all files in this repository are licensed under the
// Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/).
// Copyright 2026-Present Datadog, Inc.

//! Every pair under `examples/lean/` gets the verdict its `-- expect:` line declares. That line is
//! the first after the license header, in the file's leading comment block.
//!
//! This runs real Lean, so it needs `lake` (or `$LAKE`) and fails without it. It does not skip: a
//! test that passes when Lean is missing would say nothing about the proofs.

use std::path::Path;
use std::time::Duration;

use sqleq_lean::translate::translate;
use sqleq_lean::{check, run::Lean, Case, PROVED};

fn examples() -> Vec<(String, String, String)> {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../examples/lean");
    let mut out = Vec::new();
    for e in std::fs::read_dir(&dir).unwrap() {
        let p = e.unwrap().path();
        if p.extension().is_some_and(|x| x == "sql") {
            let text = std::fs::read_to_string(&p).unwrap();
            // Only the leading comment block is searched, so a statement cannot carry the line.
            let expect = text
                .lines()
                .take_while(|l| l.starts_with("--") || l.trim().is_empty())
                .find_map(|l| l.strip_prefix("-- expect: "))
                .unwrap_or_else(|| panic!("{} has no `-- expect:` line in its header", p.display()))
                .trim()
                .to_string();
            out.push((p.file_name().unwrap().to_string_lossy().into(), text, expect));
        }
    }
    out.sort();
    out
}

#[test]
fn examples_get_their_declared_verdicts() {
    let ex = examples();
    assert!(ex.len() >= 8, "expected the golden pairs, found {}", ex.len());
    let cases: Vec<Case> = ex.iter().map(|(n, t, _)| Case::from_file(n.clone(), t)).collect();
    let lean = Lean::from_env(Duration::from_secs(600));
    let got = check(&cases, &lean, 50, 2, None, false).expect("Lean must be available: set $LAKE or put lake on PATH");
    let mut proved = 0;
    for ((name, _, expect), (_, rec)) in ex.iter().zip(&got) {
        assert_eq!(rec["verdict"].as_str().unwrap(), expect, "{name}: {rec}");
        proved += (expect == PROVED) as usize;
    }
    assert!(proved >= 3, "the positive examples must actually reach the kernel");
}

/// A pair whose two sides differ only in the tail must be refused by the *kernel* too, not only by
/// the Rust comparison. Forge a translation whose tails differ and check Lean rejects it.
#[test]
fn the_kernel_rechecks_what_rust_compared() {
    let text = std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("../examples/lean/row_major.sql")).unwrap();
    let case = Case::from_file("forged".into(), &text);
    let (a, b) = case.pair.as_ref().unwrap();
    let mut p = translate(a, b, &case.schema).unwrap();
    p.b.tail.push(sqleq_lean::translate::LTok::Word(u32::MAX));
    let (src, entries) = sqleq_lean::emit::batch(&[&p]);
    let dir = std::env::temp_dir().join(format!("sqleq-lean-forged-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("forged.lean");
    std::fs::write(&file, &src).unwrap();
    let lean = Lean::from_env(Duration::from_secs(600));
    lean.build().unwrap();
    let out = lean.check(&file).unwrap().expect("no timeout");
    let _ = std::fs::remove_dir_all(&dir);
    // The forged file is still well-formed, so the audit passes it: refusing it is the kernel's job.
    sqleq_lean::run::audit(&src).expect("a forged tail is still a well-formed batch");
    let o = sqleq_lean::run::outcomes(&src, &out, &[entries[0].proof.clone()]);
    assert!(matches!(&o[0], sqleq_lean::run::Outcome::Failed(_)), "forged pair was proved: {out}");
}

/// The audit accepts what the emitter writes for every example, and refuses each way a batch could
/// prove something other than `EquivGather A B` of its own pair, or change how it is checked.
#[test]
fn the_audit_accepts_emitted_batches_and_refuses_tampered_ones() {
    use sqleq_lean::run::audit;
    let pairs: Vec<_> = examples()
        .iter()
        .filter_map(|(n, t, _)| {
            let c = Case::from_file(n.clone(), t);
            let (a, b) = c.pair.as_ref().ok()?;
            translate(a, b, &c.schema).ok()
        })
        .collect();
    assert!(pairs.len() >= 5, "examples that translate");
    let (src, _) = sqleq_lean::emit::batch(&pairs.iter().collect::<Vec<_>>());
    audit(&src).expect("the emitter's own output passes");

    let tamper: [(&str, &str); 8] = [
        ("theorem equiv : EquivGather A B := checkGather_sound tys A B ok", "theorem equiv : True := trivial"),
        ("theorem equiv : EquivGather A B := checkGather_sound tys A B ok", "theorem equiv : EquivGather B A := checkGather_sound tys A B ok"),
        ("open Sqleq\n", "open Sqleq\nset_option debug.skipKernelTC true\n"),
        ("open Sqleq\n", "open Sqleq\naxiom cheat : False\n"),
        ("noncomputable def A : Insert := ", "noncomputable def A : Insert := Sqleq.Controls.rowMajor ++ "),
        ("theorem ok : checkGather tys A B = true := by decide +kernel", "theorem ok : checkGather tys A B = true := by native_decide"),
        ("end Q0\n", "end Q1\n"),
        ("#print axioms Q0.equiv", "#print axioms Q1.equiv"),
    ];
    for (from, to) in tamper {
        assert!(src.contains(from), "{from:?} not in the emitted batch");
        let bad = src.replacen(from, to, 1);
        assert!(audit(&bad).is_err(), "the audit passed a batch with {to:?}");
    }
    assert!(audit(&format!("{src}\n#eval 1\n")).is_err(), "a trailing command");
}
