// Unless explicitly stated otherwise all files in this repository are licensed under the
// Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/).
// Copyright 2026-Present Datadog, Inc.

//! Lean source for a batch of pairs.
//!
//! One file holds many pairs, because a Lean process spends about half a second importing the
//! library before it checks anything. Each pair `i` gets:
//! - namespace `Q<i>`: `A`, `B`, the column types and the witness spec as definitions, `ok` (the
//!   kernel runs the checker), and `equiv` (the soundness theorem applied to `ok`);
//! - namespace `W<i>`: `wit`, the kernel's canonical run of `A` (see `Sqleq.Witness`).
//!
//! Each is followed by `#print axioms`, which is what the runner reads. They sit in separate line
//! ranges, so a failed witness is never mistaken for a failed proof.
//!
//! Definitions are `noncomputable` and constructors fully qualified: the kernel never needs
//! compiled code, and on a 60-row pair those two choices cut elaboration and kernel time sharply.

use std::fmt::Write;
use std::ops::Range;

use crate::schema::DefaultKind;
use crate::translate::{LCell, LConflict, LInsert, LSource, LSpec, LTok, LeanPair, Witness};

fn list<T>(xs: &[T], f: impl Fn(&T) -> String) -> String {
    let items: Vec<String> = xs.iter().map(f).collect();
    format!("[{}]", items.join(", "))
}

fn nats<T: std::fmt::Display>(xs: &[T]) -> String {
    list(xs, |x| x.to_string())
}

fn cell(c: &LCell) -> String {
    match c {
        LCell::P(n) => format!("Sqleq.Cell.p {n}"),
        LCell::Pc(n, t) => format!("Sqleq.Cell.pc {n} {t}"),
        LCell::Null => "Sqleq.Cell.null".into(),
    }
}

fn tok(t: &LTok) -> String {
    match t {
        LTok::Word(n) => format!("Sqleq.Tok.word {n}"),
        LTok::Param(n) => format!("Sqleq.Tok.param {n}"),
    }
}

fn insert(i: &LInsert) -> String {
    let src = match &i.src {
        LSource::Values(rows) => format!("Sqleq.Source.values {}", list(rows, |r| list(r, cell))),
        LSource::Unnest(args) => {
            format!("Sqleq.Source.unnest {}", list(args, |(p, t)| format!("Sqleq.Arg.mk {p} {t}")))
        }
    };
    format!("Sqleq.Insert.mk {} {} ({src}) {}", i.target, nats(&i.cols), list(&i.tail, tok))
}

fn spec(s: &LSpec) -> String {
    let col = |(nullable, d, always): &(bool, DefaultKind, bool)| {
        let d = match d {
            DefaultKind::Null => "null",
            DefaultKind::Same => "same",
            DefaultKind::Fresh => "fresh",
        };
        format!("Sqleq.Col.mk {nullable} Sqleq.Dflt.{d} {always}")
    };
    let uniq = |(cols, nnd): &(Vec<usize>, bool)| format!("Sqleq.Uniq.mk {} {nnd}", nats(cols));
    let conflict = match s.conflict {
        LConflict::None => "Sqleq.Conflict.none".to_string(),
        LConflict::Nothing => "Sqleq.Conflict.nothing".to_string(),
        LConflict::NothingOn(u) => format!("(Sqleq.Conflict.nothingOn {u})"),
        LConflict::Update(u) => format!("(Sqleq.Conflict.update {u})"),
        LConflict::NoArbiter => "Sqleq.Conflict.noArbiter".to_string(),
    };
    format!(
        "Sqleq.Spec.mk {} {} {} {conflict}",
        list(&s.cols, col),
        list(&s.uniques, uniq),
        nats(&s.ins)
    )
}

/// The namespaces a batch entry is emitted under.
pub fn namespace(i: usize) -> String {
    format!("Q{i}")
}
pub fn witness_namespace(i: usize) -> String {
    format!("W{i}")
}

/// Where one entry's proof and witness sit in the file (0-based lines, end exclusive), and the
/// theorem whose axioms the runner must read for each.
pub struct Entry {
    pub proof: (Range<usize>, String),
    pub witness: Option<(Range<usize>, String)>,
}

fn defs(p: &LeanPair) -> String {
    let mut out = format!(
        "noncomputable def tys : List Nat := {}\n\
         noncomputable def A : Insert := {}\n\
         noncomputable def B : Insert := {}\n",
        nats(&p.tys),
        insert(&p.a),
        insert(&p.b)
    );
    if let Witness::Spec(s) = &p.witness {
        let _ = writeln!(out, "noncomputable def spec : Spec := {}", spec(s));
    }
    out
}

/// A batch file and where each entry sits in it.
pub fn batch(pairs: &[&LeanPair]) -> (String, Vec<Entry>) {
    let mut out = String::from("import Sqleq\nopen Sqleq\n\n");
    let mut entries = Vec::with_capacity(pairs.len());
    for (i, p) in pairs.iter().enumerate() {
        let ns = namespace(i);
        let start = out.lines().count();
        let _ = write!(
            out,
            "namespace {ns}\n{}\
             theorem ok : checkGather tys A B = true := by decide +kernel\n\
             theorem equiv : EquivGather A B := checkGather_sound tys A B ok\n\
             end {ns}\n\
             #print axioms {ns}.equiv\n",
            defs(p)
        );
        let proof = (start..out.lines().count(), format!("{ns}.equiv"));
        let witness = matches!(p.witness, Witness::Spec(_)).then(|| {
            let wns = witness_namespace(i);
            let ws = out.lines().count();
            let _ = write!(
                out,
                "namespace {wns}\n\
                 theorem wit : (witness {ns}.spec {ns}.A).isOk = true := by decide +kernel\n\
                 end {wns}\n\
                 #print axioms {wns}.wit\n"
            );
            (ws..out.lines().count(), format!("{wns}.wit"))
        });
        out.push('\n');
        entries.push(Entry { proof, witness });
    }
    (out, entries)
}

/// A file that prints, for each `(tag, pair)`, the canonical run's outcome as `(tag, WResult…)`.
/// Only for pairs whose witness failed, which are almost always small: `#reduce` runs in the
/// elaborator, not the kernel, and needs a raised recursion limit on long rows.
pub fn reasons(pairs: &[(usize, &LeanPair)]) -> String {
    let mut out = String::from("import Sqleq\nopen Sqleq\nset_option maxRecDepth 100000\n\n");
    for (tag, p) in pairs {
        let ns = format!("R{tag}");
        let _ = write!(out, "namespace {ns}\n{}end {ns}\n#reduce ({tag}, witness {ns}.spec {ns}.A)\n\n", defs(p));
    }
    out
}
