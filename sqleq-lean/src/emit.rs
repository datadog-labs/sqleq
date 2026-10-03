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

use std::fmt::{self, Display, Write};
use std::ops::Range;

use crate::recognize::GenKind;
use crate::schema::DefaultKind;
use crate::translate::{LCell, LConflict, LInsert, LSource, LSpec, LTok, LeanPair, Witness};

// Each of the translator's Lean-side types renders as the Lean term it stands for. They have no
// other textual form, so `Display` is that term.

/// A Lean list literal, `[a, b, c]`.
struct List<'a, T>(&'a [T]);

impl<T: Display> Display for List<'_, T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("[")?;
        for (i, x) in self.0.iter().enumerate() {
            if i > 0 {
                f.write_str(", ")?;
            }
            write!(f, "{x}")?;
        }
        f.write_str("]")
    }
}

impl Display for LCell {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            LCell::P(n) => write!(f, "Sqleq.Cell.p {n}"),
            LCell::Pc(n, t) => write!(f, "Sqleq.Cell.pc {n} {t}"),
            LCell::Null => f.write_str("Sqleq.Cell.null"),
            LCell::Gen(k) => {
                let k = match k {
                    GenKind::Default => "dflt",
                    GenKind::Fresh => "fresh",
                    GenKind::Once => "once",
                };
                write!(f, "Sqleq.Cell.gen Sqleq.GenKind.{k}")
            }
        }
    }
}

/// The checker, its soundness theorem and the claim a pair is proved under: `checkGather` for a
/// pair whose `VALUES` side has only parameters and `NULL`s, `checkGatherGen` for one with generated
/// cells.
pub fn claim_lines(generated: bool) -> [&'static str; 2] {
    if generated {
        [
            "theorem ok : checkGatherGen tys A B = true := by decide +kernel",
            "theorem equiv : EquivGatherGen A B := checkGatherGen_sound tys A B ok",
        ]
    } else {
        [
            "theorem ok : checkGather tys A B = true := by decide +kernel",
            "theorem equiv : EquivGather A B := checkGather_sound tys A B ok",
        ]
    }
}

impl Display for LTok {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            LTok::Word(n) => write!(f, "Sqleq.Tok.word {n}"),
            LTok::Param(n) => write!(f, "Sqleq.Tok.param {n}"),
        }
    }
}

impl Display for LSource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            LSource::Values(rows) => {
                let rows: Vec<List<'_, LCell>> = rows.iter().map(|r| List(r)).collect();
                write!(f, "Sqleq.Source.values {}", List(&rows))
            }
            LSource::Unnest(args) => {
                let args: Vec<Arg> = args.iter().map(|&(p, t)| Arg(p, t)).collect();
                write!(f, "Sqleq.Source.unnest {}", List(&args))
            }
        }
    }
}

/// One unnest argument, `Sqleq.Arg.mk param type`.
struct Arg(u32, u32);

impl Display for Arg {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Sqleq.Arg.mk {} {}", self.0, self.1)
    }
}

impl Display for LInsert {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Sqleq.Insert.mk {} {} ({}) {}", self.target, List(&self.cols), self.src, List(&self.tail))
    }
}

/// One table column of the witness spec, `Sqleq.Col.mk nullable default always`.
struct Col(bool, DefaultKind, bool);

impl Display for Col {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let d = match self.1 {
            DefaultKind::Null => "null",
            DefaultKind::Same => "same",
            DefaultKind::Fresh => "fresh",
        };
        write!(f, "Sqleq.Col.mk {} Sqleq.Dflt.{d} {}", self.0, self.2)
    }
}

/// One unique constraint of the witness spec, `Sqleq.Uniq.mk cols nullsNotDistinct`.
struct Uniq<'a>(&'a [usize], bool);

impl Display for Uniq<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Sqleq.Uniq.mk {} {}", List(self.0), self.1)
    }
}

impl Display for LConflict {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            LConflict::None => f.write_str("Sqleq.Conflict.none"),
            LConflict::Nothing => f.write_str("Sqleq.Conflict.nothing"),
            LConflict::NothingOn(u) => write!(f, "(Sqleq.Conflict.nothingOn {u})"),
            LConflict::Update(u) => write!(f, "(Sqleq.Conflict.update {u})"),
            LConflict::NoArbiter => f.write_str("Sqleq.Conflict.noArbiter"),
        }
    }
}

impl Display for LSpec {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let cols: Vec<Col> = self.cols.iter().map(|&(n, d, a)| Col(n, d, a)).collect();
        let uniques: Vec<Uniq<'_>> = self.uniques.iter().map(|(c, nnd)| Uniq(c, *nnd)).collect();
        write!(f, "Sqleq.Spec.mk {} {} {} {}", List(&cols), List(&uniques), List(&self.ins), self.conflict)
    }
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
        List(&p.tys),
        p.a,
        p.b
    );
    if let Witness::Spec(s) = &p.witness {
        let _ = writeln!(out, "noncomputable def spec : Spec := {s}");
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
        let [ok, equiv] = claim_lines(p.generated.is_some());
        let _ = write!(out, "namespace {ns}\n{}{ok}\n{equiv}\nend {ns}\n#print axioms {ns}.equiv\n", defs(p));
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_constructor_renders_as_its_lean_term() {
        let a = LInsert {
            target: 7,
            cols: vec![1, 2],
            src: LSource::Values(vec![vec![LCell::P(1), LCell::Pc(2, 3)], vec![LCell::Null, LCell::P(4)]]),
            tail: vec![LTok::Word(5), LTok::Param(6)],
        };
        assert_eq!(
            a.to_string(),
            "Sqleq.Insert.mk 7 [1, 2] (Sqleq.Source.values [[Sqleq.Cell.p 1, Sqleq.Cell.pc 2 3], \
             [Sqleq.Cell.null, Sqleq.Cell.p 4]]) [Sqleq.Tok.word 5, Sqleq.Tok.param 6]"
        );
        assert_eq!(
            [LCell::Gen(GenKind::Default), LCell::Gen(GenKind::Fresh), LCell::Gen(GenKind::Once)]
                .iter()
                .map(|c| c.to_string())
                .collect::<Vec<_>>(),
            ["Sqleq.Cell.gen Sqleq.GenKind.dflt", "Sqleq.Cell.gen Sqleq.GenKind.fresh", "Sqleq.Cell.gen Sqleq.GenKind.once"]
        );
        assert_eq!(LSource::Unnest(vec![(1, 3), (2, 4)]).to_string(), "Sqleq.Source.unnest [Sqleq.Arg.mk 1 3, Sqleq.Arg.mk 2 4]");
        assert_eq!(LSource::Values(vec![]).to_string(), "Sqleq.Source.values []");
        let spec = LSpec {
            cols: vec![(true, DefaultKind::Null, false), (false, DefaultKind::Fresh, true), (false, DefaultKind::Same, false)],
            uniques: vec![(vec![0, 2], true)],
            ins: vec![0, 2],
            conflict: LConflict::NothingOn(0),
        };
        assert_eq!(
            spec.to_string(),
            "Sqleq.Spec.mk [Sqleq.Col.mk true Sqleq.Dflt.null false, Sqleq.Col.mk false Sqleq.Dflt.fresh true, \
             Sqleq.Col.mk false Sqleq.Dflt.same false] [Sqleq.Uniq.mk [0, 2] true] [0, 2] (Sqleq.Conflict.nothingOn 0)"
        );
        let c = [LConflict::None, LConflict::Nothing, LConflict::Update(1), LConflict::NoArbiter];
        assert_eq!(
            c.iter().map(|c| c.to_string()).collect::<Vec<_>>(),
            ["Sqleq.Conflict.none", "Sqleq.Conflict.nothing", "(Sqleq.Conflict.update 1)", "Sqleq.Conflict.noArbiter"]
        );
    }
}
