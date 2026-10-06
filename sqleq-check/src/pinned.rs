// Unless explicitly stated otherwise all files in this repository are licensed under the
// Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/).
// Copyright 2026-Present Datadog, Inc.

//! Pinned mode: each axis's answer, reduced to the one word [`crate::suite::words`] lets a case
//! pin. A prover asked about a pair the frontend refused has nothing to say about it, and says
//! `no-plan` -- the refusal itself is the frontend axis's answer.

use std::collections::HashMap;
use std::path::Path;

use crate::case::{Case, ERROR, PANIC, PROVABLE, REFUSED, TIMEOUT, UNPROVABLE};
use crate::suite::{self, Header, Judgement};

fn qed_word(status: &str) -> &'static str {
    match status {
        PROVABLE => "proved",
        UNPROVABLE => "no-proof",
        PANIC => "panic",
        TIMEOUT => "timeout",
        ERROR => "error",
        _ => "error",
    }
}

/// axis -> (word, note) for every axis in `axes` that this run asked about the case.
pub fn observe(case: &Case, axes: &[&str]) -> HashMap<String, (String, String)> {
    let mut out = HashMap::new();
    let mut put = |axis: &str, word: &str, note: &str| {
        out.insert(axis.to_string(), (word.to_string(), note.to_string()));
    };
    if axes.contains(&"frontend") {
        if case.lowered {
            put("frontend", if case.trivial == Some(true) { "emit-reflexive" } else { "emit" }, "");
        } else if case.status == REFUSED && case.reflexive {
            // Refused, but the two sides normalize to one query: settled all the same, and the
            // refusal's reason kept as the note.
            put("frontend", "reflexive", &case.message);
        } else if case.status == REFUSED {
            put("frontend", &format!("refuse:{}", case.refuse_kind), &case.message);
        } else if case.status == TIMEOUT {
            put("frontend", "timeout", &case.message);
        } else {
            put("frontend", "missing", &case.message);
        }
    }
    let no_plan = !case.lowered;
    if axes.contains(&"qed") {
        if no_plan {
            put("qed", "no-plan", "");
        } else {
            let mut word = qed_word(&case.status);
            if word == "proved" && case.trivial == Some(true) {
                word = "proved-literal";
            }
            put("qed", word, &case.message);
        }
    }
    for axis in ["sqleq-solver", "sqlsolver-jvm"] {
        if axes.contains(&axis) {
            if no_plan {
                put(axis, "no-plan", "");
            } else {
                put(axis, case.s_bucket.as_deref().unwrap_or(crate::axes::solver::MISSING), &case.s_note);
            }
        }
    }
    if axes.contains(&"fuzz") {
        put("fuzz", case.f_verdict.as_deref().unwrap_or("missing"), &case.f_note);
    }
    if axes.contains(&"lean") {
        put("lean", case.l_verdict.as_deref().unwrap_or("missing"), &case.l_reason);
    }
    out
}

pub struct Pinned {
    pub case: usize,
    pub header: Header,
    pub lint: Vec<String>,
    pub judgements: Vec<Judgement>,
}

impl Pinned {
    pub fn passed(&self) -> bool {
        self.lint.is_empty() && self.judgements.iter().all(Judgement::passed)
    }
}

pub fn judge_cases(cases: &[Case], axes: &[&str]) -> Vec<Pinned> {
    cases
        .iter()
        .enumerate()
        .map(|(i, x)| {
            let text = std::fs::read_to_string(&x.path).unwrap_or_default();
            let h = suite::parse_header(&text);
            let errs = suite::lint(&h);
            let judgements = if errs.is_empty() { suite::judge(&h, &observe(x, axes)) } else { Vec::new() };
            Pinned { case: i, header: h, lint: errs, judgements }
        })
        .collect()
}

/// Rewrite the `expect` lines of every lint-clean case; the names of the files changed.
pub fn bless(pinned: &[Pinned], cases: &[Case]) -> Result<Vec<String>, String> {
    let mut changed = Vec::new();
    for p in pinned {
        if !p.lint.is_empty() {
            continue;
        }
        let x = &cases[p.case];
        let path = Path::new(&x.path);
        let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
        let new = suite::bless_text(&text, &p.header, &p.judgements);
        if suite::write_if_changed(path, &new).map_err(|e| format!("{}: {e}", path.display()))? {
            changed.push(x.name.clone());
        }
    }
    Ok(changed)
}
