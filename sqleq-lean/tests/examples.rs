// Unless explicitly stated otherwise all files in this repository are licensed under the
// Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/).
// Copyright 2026-Present Datadog, Inc.

//! Every pinned pair under `tests/pairs/` that pins the Lean axis gets the verdict its
//! `-- expect lean:` line declares (see `tests/pairs/README.md`). The line is read from the file's
//! leading comment block; a `!known-unsound` after the verdict is not part of it.
//!
//! This runs real Lean, so it needs `lake` (or `$LAKE`) and fails without it. It does not skip: a
//! test that passes when Lean is missing would say nothing about the proofs.

use std::path::{Path, PathBuf};
use std::time::Duration;

use sqleq_lean::translate::translate;
use sqleq_lean::{check, run::Lean, Case, PROVED};

fn pairs_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../tests/pairs")
}

fn sql_files(dir: &Path, out: &mut Vec<PathBuf>) {
    for e in std::fs::read_dir(dir).unwrap() {
        let p = e.unwrap().path();
        if p.is_dir() {
            sql_files(&p, out);
        } else if p.extension().is_some_and(|x| x == "sql") {
            out.push(p);
        }
    }
}

/// (name relative to `tests/pairs/`, file text, pinned Lean verdict), for every pair that pins one.
fn examples() -> Vec<(String, String, String)> {
    let root = pairs_dir();
    let mut files = Vec::new();
    sql_files(&root, &mut files);
    let mut out = Vec::new();
    for p in files {
        let text = std::fs::read_to_string(&p).unwrap();
        // Only the leading comment block is searched, so a statement cannot carry the line.
        let expect = text
            .lines()
            .take_while(|l| l.starts_with("--") || l.trim().is_empty())
            .find_map(|l| l.strip_prefix("-- expect lean: "))
            .and_then(|v| v.split_whitespace().next())
            .map(str::to_string);
        if let Some(expect) = expect {
            let name = p.strip_prefix(&root).unwrap().to_string_lossy().into_owned();
            out.push((name, text, expect));
        }
    }
    out.sort();
    out
}

#[test]
fn examples_get_their_declared_verdicts() {
    let ex = examples();
    assert!(ex.len() >= 8, "expected the pinned pairs, found {}", ex.len());
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
    let text = std::fs::read_to_string(pairs_dir().join("insert_unnest/row_major.sql")).unwrap();
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

/// A pair with generated cells (`DEFAULT` on a serial key), and one without.
const GENERATED: &str = "CREATE TABLE g (id serial PRIMARY KEY, name text);
INSERT INTO g (id, name) VALUES (DEFAULT, $1), (DEFAULT, $2);
INSERT INTO g (id, name) SELECT * FROM unnest($1::int[], $2::text[]);";
const PLAIN: &str = "CREATE TABLE g (id serial PRIMARY KEY, name text);
INSERT INTO g (id, name) VALUES ($1, $2), ($3, $4);
INSERT INTO g (id, name) SELECT * FROM unnest($1::int[], $2::text[]);";

fn lean_pair(text: &str) -> sqleq_lean::translate::LeanPair {
    let case = Case::from_file("pair".into(), text);
    let (a, b) = case.pair.as_ref().unwrap();
    translate(a, b, &case.schema).unwrap()
}

/// Lean's output on `src`, written to a file of its own.
fn kernel(src: &str, tag: &str) -> String {
    let dir = std::env::temp_dir().join(format!("sqleq-lean-{tag}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("batch.lean");
    std::fs::write(&file, src).unwrap();
    let lean = Lean::from_env(Duration::from_secs(600));
    lean.build().unwrap();
    let out = lean.check(&file).unwrap().expect("no timeout");
    let _ = std::fs::remove_dir_all(&dir);
    out
}

#[test]
fn a_generated_pair_is_proved_under_its_own_claim() {
    let lean = Lean::from_env(Duration::from_secs(600));
    let got = check(&[Case::from_file("g".into(), GENERATED)], &lean, 50, 1, None, false).unwrap();
    let rec = &got[0].1;
    assert_eq!(rec["verdict"], sqleq_lean::PROVED_GENERATED, "{rec}");
    assert_eq!(rec["generated"]["sequence"], true, "{rec}");
}

/// Which claim a pair is proved under is the kernel's to check, not the translator's: a pair with
/// generated cells emitted under `checkGather`, and one without emitted under `checkGatherGen`,
/// are both well-formed batches that the kernel must refuse.
#[test]
fn the_kernel_checks_which_claim_a_pair_is_proved_under() {
    for (text, generated) in [(GENERATED, true), (PLAIN, false)] {
        let mut p = lean_pair(text);
        assert_eq!(p.generated.is_some(), generated);
        p.generated = if generated { None } else { Some(Default::default()) };
        let (src, entries) = sqleq_lean::emit::batch(&[&p]);
        let out = kernel(&src, "claim");
        sqleq_lean::run::audit(&src).expect("the forgery is a well-formed batch");
        let o = sqleq_lean::run::outcomes(&src, &out, &[entries[0].proof.clone()]);
        assert!(matches!(&o[0], sqleq_lean::run::Outcome::Failed(_)), "forged claim was proved: {out}");
    }
}

/// The audit reports each entry's claim, in order, and refuses a batch whose two claim lines
/// disagree or whose data names a constructor the package does not have.
#[test]
fn the_audit_reads_each_entrys_claim() {
    use sqleq_lean::run::{audit, Claim};
    let (g, p) = (lean_pair(GENERATED), lean_pair(PLAIN));
    let (src, _) = sqleq_lean::emit::batch(&[&p, &g, &p]);
    assert_eq!(audit(&src).unwrap(), [Claim::Gather, Claim::GatherGenerated, Claim::Gather]);
    let tamper = [
        (
            "theorem equiv : EquivGatherGen A B := checkGatherGen_sound tys A B ok",
            "theorem equiv : EquivGather A B := checkGather_sound tys A B ok",
        ),
        ("Sqleq.GenKind.dflt", "Sqleq.GenKind.other"),
    ];
    for (from, to) in tamper {
        assert!(src.contains(from), "{from:?} not in the emitted batch");
        assert!(audit(&src.replacen(from, to, 1)).is_err(), "the audit passed a batch with {to:?}");
    }
}
