// Unless explicitly stated otherwise all files in this repository are licensed under the
// Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/).
// Copyright 2026-Present Datadog, Inc.

//! The pinned-pair suite: header grammar, lint, judgement and `--bless`.
//!
//! A pinned pair is an ordinary pair file -- DDL and two statements -- whose leading comment block
//! says what the pair *is* and what each axis said about it:
//!
//! ```text
//! -- truth: not-equivalent
//! -- expect frontend: emit
//! -- expect fuzz: counterexample
//! -- expect sqleq-solver: no-proof
//! -- expect qed: proved !known-unsound
//! -- origin: why this pair is here
//! -- witness: the instance on which the two sides differ
//! ```
//!
//! `truth` is written by a person and nothing here ever writes it. The `expect` lines are a
//! ratchet: `sqleq-check --expect pinned` fails on *any* movement, an improvement included, and
//! `--bless` rewrites them so the move is reviewed as a diff. What `--bless` cannot do is pin an
//! answer that contradicts `truth` -- a proof of a non-equivalent pair, a counterexample to an
//! equivalent one. Those fail every run until the bug is fixed, or until a person marks that one
//! line `!known-unsound`, which turns it into a strict expected failure: it passes while the bug
//! reproduces and fails the run that fixes it, so the marker cannot outlive the bug.
//!
//! `truth` is stated under a parameter binding. The default, `index`, is the one every axis but
//! Lean answers under: `$N` on one side is `$N` on the other. A pair headed `-- binding: gather`
//! states its truth under the gather rule instead (the `unnest` side's array `$j` is column `j` of
//! the `VALUES` rows; docs/LEAN.md), which only the Lean axis answers under. An answer can
//! contradict a truth only when the axis and the pair use the same binding; under the other one it
//! is an ordinary pin.
//!
//! This is the logic only; the harness runs the axes and calls it.

use std::collections::HashMap;
use std::io::Write as _;
use std::path::Path;

pub const EQUIVALENT: &str = "equivalent";
pub const NOT_EQUIVALENT: &str = "not-equivalent";
pub const TRUTHS: [&str; 2] = [EQUIVALENT, NOT_EQUIVALENT];

/// Canonical order: the order `--bless` inserts missing lines in, and the table's column order.
pub const AXES: [&str; 6] = ["frontend", "fuzz", "qed", "sqleq-solver", "sqlsolver-jvm", "lean"];
pub const PROVERS: [&str; 3] = ["qed", "sqleq-solver", "sqlsolver-jvm"];

/// An axis name as [`AXES`] spells it. `sqlsolver-rust` is sqleq-solver's axis from before the
/// rename, still read wherever an axis is named -- `--axes` and `expect` lines -- so a pin or a
/// script written before it keeps working. A pin `--bless` rewrites gets the new name.
pub fn canonical_axis(name: &str) -> &str {
    if name == "sqlsolver-rust" {
        "sqleq-solver"
    } else {
        name
    }
}

pub const INDEX: &str = "index";
pub const GATHER: &str = "gather";
pub const GATHER_GENERATED: &str = "gather-generated";
pub const BINDINGS: [&str; 3] = [INDEX, GATHER, GATHER_GENERATED];
pub const GATHERS: [&str; 2] = [GATHER, GATHER_GENERATED];

/// The bindings an axis answers under. Only Lean reads a scalar and an array at the same `$N` as
/// the gather rule relates them; every other axis refuses such a pair.
pub fn axis_bindings(axis: &str) -> &'static [&'static str] {
    if axis == "lean" {
        &GATHERS
    } else if AXES.contains(&axis) {
        &[INDEX]
    } else {
        &[]
    }
}

pub const PROVED_WORDS: [&str; 2] = ["proved", "proved-literal"];

/// The frontend's two ways of finding the sides one query: `emit-reflexive`, lowered to the same
/// IR, and `reflexive`, refused but normalized to the same tree. Either settles the pair without a
/// prover, so either is a claim of equivalence -- and against a fuzz counterexample, an alarm.
pub const FRONTEND_SAME: [&str; 2] = ["emit-reflexive", "reflexive"];

/// What may be pinned, per axis. Only the stable *kind* of an answer is pinned, never its message,
/// so rewording a refusal does not move a pin and changing what is refused does.
pub fn words(axis: &str) -> &'static [&'static str] {
    match axis {
        "frontend" => &[
            "emit",
            "emit-reflexive",
            "reflexive",
            "refuse:parse",
            "refuse:unsupported",
            "refuse:schema",
            "refuse:parameter-misaligned",
        ],
        "fuzz" => &[
            "counterexample",
            "no-counterexample",
            "param-misaligned",
            "not-comparable",
            "nondet-skip",
            "no-schema",
            "no-tables",
            "error",
        ],
        "qed" => &["proved", "proved-literal", "no-proof", "no-plan", "panic", "error"],
        "sqleq-solver" | "sqlsolver-jvm" => {
            &["proved", "proved-literal", "no-proof", "unsupported", "no-plan", "error"]
        }
        // `no-witness` is a kernel proof too, only possibly vacuous, so it is a claim of
        // equivalence.
        "lean" => &[
            "proved-gather",
            "no-witness",
            "proved-gather-generated",
            "no-witness-generated",
            "unsupported",
            "invalid-sql",
            "error",
        ],
        _ => &[],
    }
}

/// The answers that claim equivalence under `binding`. Under `gather-generated`, Lean's
/// `*-generated` answers claim the relation the truth is stated in, and a plain gather proof claims
/// more than that, so it counts too. Under `gather` a generated answer claims less than the truth,
/// so it does not.
pub fn claims_equivalent(axis: &str, binding: &str) -> &'static [&'static str] {
    if binding == GATHER_GENERATED {
        return match axis {
            "lean" => &["proved-gather-generated", "no-witness-generated", "proved-gather", "no-witness"],
            _ => &[],
        };
    }
    match axis {
        "qed" | "sqleq-solver" | "sqlsolver-jvm" => &PROVED_WORDS,
        "frontend" => &FRONTEND_SAME,
        "lean" => &["proved-gather", "no-witness"],
        _ => &[],
    }
}

/// Evidence for an equivalent truth: a claim that is not possibly vacuous, which `no-witness` is.
pub fn evidence_equivalent(axis: &str, binding: &str) -> &'static [&'static str] {
    if binding == GATHER_GENERATED {
        return match axis {
            "lean" => &["proved-gather-generated", "proved-gather"],
            _ => &[],
        };
    }
    match axis {
        "qed" | "sqleq-solver" | "sqlsolver-jvm" => &PROVED_WORDS,
        "frontend" => &FRONTEND_SAME,
        "lean" => &["proved-gather"],
        _ => &[],
    }
}

/// The one answer that claims the opposite of equivalence.
pub fn refutes(axis: &str) -> &'static [&'static str] {
    match axis {
        "fuzz" => &["counterexample"],
        _ => &[],
    }
}

/// Never pinnable: each says the run did not get an answer, not what the answer was.
pub const UNPINNABLE: [&str; 2] = ["timeout", "missing"];

/// The frontend flags each `-- catalog:` value asks for, in the order the names are listed.
pub const CATALOGS: [(&str, &[&str]); 3] =
    [("declared", &[]), ("inferred", &["--infer"]), ("inferred-seeded", &["--infer-seeded"])];

pub fn catalog_flags(name: &str) -> Option<&'static [&'static str]> {
    CATALOGS.iter().find(|(n, _)| *n == name).map(|(_, f)| *f)
}

pub const MARKER: &str = "!known-unsound";
pub const TEXT_KEYS: [&str; 6] = ["truth", "binding", "catalog", "origin", "witness", "argument"];

/// Python's `str.splitlines()`: every line boundary it knows, and no empty last line for a final
/// terminator. `bless_text` rewrites by line index, so it has to cut the file where the rest of
/// the suite's history cut it.
pub fn splitlines(text: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut start = 0;
    let mut it = text.char_indices().peekable();
    while let Some((i, ch)) = it.next() {
        let boundary = matches!(
            ch,
            '\n' | '\r' | '\u{0b}' | '\u{0c}' | '\u{1c}' | '\u{1d}' | '\u{1e}' | '\u{85}' | '\u{2028}' | '\u{2029}'
        );
        if !boundary {
            continue;
        }
        out.push(&text[start..i]);
        let mut end = i + ch.len_utf8();
        if ch == '\r' {
            if let Some(&(j, '\n')) = it.peek() {
                it.next();
                end = j + 1;
            }
        }
        start = end;
    }
    if start < text.len() {
        out.push(&text[start..]);
    }
    out
}

/// A directive is `-- key: value` with a lowercase key right after `-- `: one word or two, each
/// `[a-z][a-z0-9-]*`. Prose in the header starts with a capital or with more indentation, so a typo
/// such as `-- expect fuz:` is a lint error instead of a comment nobody reads.
fn directive(line: &str) -> Option<(&str, &str)> {
    let rest = line.strip_prefix("-- ")?;
    let colon = rest.find(':')?;
    let key = &rest[..colon];
    let mut parts = key.split(' ');
    let ok_word = |w: &str| {
        let mut cs = w.chars();
        matches!(cs.next(), Some('a'..='z'))
            && cs.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
    };
    let first = parts.next()?;
    if !ok_word(first) {
        return None;
    }
    if let Some(second) = parts.next() {
        if !ok_word(second) || parts.next().is_some() {
            return None;
        }
    }
    let after = &rest[colon + 1..];
    if after.is_empty() {
        Some((key, ""))
    } else {
        after.strip_prefix(' ').map(|v| (key, v))
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Pin {
    pub word: String,
    pub marker: bool,
    /// Index into the file's lines.
    pub line: usize,
}

#[derive(Clone, Debug)]
pub struct Header {
    pub truth: Option<String>,
    pub binding: String,
    pub catalog: String,
    /// Key -> value, for every [`TEXT_KEYS`] key present.
    pub text: HashMap<String, String>,
    /// Key -> line index, for every directive.
    pub lines: HashMap<String, usize>,
    /// Axis -> pin, in the order the lines appear.
    pub expect: Vec<(String, Pin)>,
    pub errors: Vec<String>,
}

impl Default for Header {
    fn default() -> Header {
        Header {
            truth: None,
            binding: INDEX.to_string(),
            catalog: "declared".to_string(),
            text: HashMap::new(),
            lines: HashMap::new(),
            expect: Vec::new(),
            errors: Vec::new(),
        }
    }
}

impl Header {
    pub fn pin(&self, axis: &str) -> Option<&Pin> {
        self.expect.iter().find(|(a, _)| a == axis).map(|(_, p)| p)
    }
}

/// The index of the first line past the leading comment block -- the same rule as
/// `sqleq-lean/tests/examples.rs`: comment lines and blank lines, up to the first SQL.
fn block_end(lines: &[&str]) -> usize {
    lines.iter().position(|ln| !(ln.starts_with("--") || ln.trim().is_empty())).unwrap_or(lines.len())
}

/// Read the directives in the leading comment block. Grammar errors are collected in `errors`, not
/// raised: a malformed case is reported beside the others, not instead of them.
pub fn parse_header(text: &str) -> Header {
    let mut h = Header::default();
    let lines = splitlines(text);
    let end = block_end(&lines);
    for (i, raw) in lines.iter().enumerate() {
        let Some((key, value)) = directive(raw.trim_end()) else { continue };
        let value = value.trim();
        if i >= end {
            // A directive below the SQL would be silently ignored, which for a pin is worse than an
            // error: the case would look pinned and check nothing.
            if TEXT_KEYS.contains(&key) || key.starts_with("expect ") {
                h.errors.push(format!("line {}: `{key}:` below the first SQL line is not read", i + 1));
            }
            continue;
        }
        if h.lines.contains_key(key) {
            h.errors.push(format!("line {}: duplicate `{key}:`", i + 1));
            continue;
        }
        h.lines.insert(key.to_string(), i);
        if let Some(axis) = key.strip_prefix("expect ") {
            let axis = canonical_axis(axis);
            if h.pin(axis).is_some() {
                h.errors.push(format!("line {}: a second `expect` line for `{axis}`", i + 1));
                continue;
            }
            if !AXES.contains(&axis) {
                h.errors.push(format!("line {}: unknown axis `{axis}` (one of {})", i + 1, AXES.join(", ")));
                continue;
            }
            let parts: Vec<&str> = value.split_whitespace().collect();
            let marker = parts.contains(&MARKER);
            let ws: Vec<&str> = parts.into_iter().filter(|p| *p != MARKER).collect();
            if ws.len() != 1 {
                h.errors.push(format!(
                    "line {}: `expect {axis}:` takes one word, then optionally {MARKER}",
                    i + 1
                ));
                continue;
            }
            let word = ws[0];
            if !words(axis).contains(&word) {
                let why = if UNPINNABLE.contains(&word) {
                    "is never pinnable: it says the run got no answer".to_string()
                } else {
                    format!("is not one of {}", words(axis).join(", "))
                };
                h.errors.push(format!("line {}: `{word}` {why}", i + 1));
                continue;
            }
            h.expect.push((axis.to_string(), Pin { word: word.to_string(), marker, line: i }));
        } else if TEXT_KEYS.contains(&key) {
            h.text.insert(key.to_string(), value.to_string());
            match key {
                "truth" => h.truth = Some(value.to_string()),
                "catalog" => h.catalog = value.to_string(),
                "binding" => h.binding = value.to_string(),
                _ => {}
            }
        } else {
            h.errors.push(format!("line {}: unknown directive `{key}:`", i + 1));
        }
    }
    h
}

/// Whether an answer is impossible for a pair of this truth -- a soundness failure of that axis
/// (or of the frontend feeding it), as opposed to a capability move. An axis answering under
/// another binding than the one the truth is stated under contradicts nothing.
pub fn contradicts(truth: Option<&str>, axis: &str, word: &str, binding: &str) -> bool {
    if !axis_bindings(axis).contains(&binding) {
        return false;
    }
    match truth {
        Some(NOT_EQUIVALENT) => claims_equivalent(axis, binding).contains(&word),
        Some(EQUIVALENT) => refutes(axis).contains(&word),
        _ => false,
    }
}

/// Every grammar error, then the rules that make a case worth having: a truth, a reason for being
/// here, evidence for the truth, and no marker that excuses nothing.
pub fn lint(h: &Header) -> Vec<String> {
    let mut errs = h.errors.clone();
    let truth = h.truth.as_deref();
    match truth {
        None => errs.push("no `truth:` line".to_string()),
        Some(t) if !TRUTHS.contains(&t) => {
            errs.push(format!("`truth: {t}` is not one of {}", TRUTHS.join(", ")))
        }
        _ => {}
    }
    if catalog_flags(&h.catalog).is_none() {
        let names: Vec<&str> = CATALOGS.iter().map(|(n, _)| *n).collect();
        errs.push(format!("`catalog: {}` is not one of {}", h.catalog, names.join(", ")));
    }
    if !BINDINGS.contains(&h.binding.as_str()) {
        errs.push(format!("`binding: {}` is not one of {}", h.binding, BINDINGS.join(", ")));
    }
    if h.text.get("origin").is_none_or(|o| o.is_empty()) {
        errs.push("no `origin:` line saying why this pair is pinned".to_string());
    }
    let truth_word = truth.unwrap_or("None");
    for (axis, pin) in &h.expect {
        let contra = contradicts(truth, axis, &pin.word, &h.binding);
        if pin.marker && !contra {
            errs.push(format!(
                "`expect {axis}: {}` carries {MARKER}, but that answer does not contradict `truth: {truth_word}`",
                pin.word
            ));
        }
        // --bless never writes one of these, so it was written by hand. Caught here, it fails every
        // run, including the CI runs that do not ask that axis.
        if contra && !pin.marker {
            errs.push(format!(
                "`expect {axis}: {}` contradicts `truth: {truth_word}`; mark it {MARKER} if that is a known bug",
                pin.word
            ));
        }
    }
    // Evidence for the truth counts only from an axis answering under the pair's binding.
    let says = |ws: &dyn Fn(&str) -> &'static [&'static str]| {
        h.expect.iter().any(|(a, p)| {
            ws(a).contains(&p.word.as_str()) && !p.marker && axis_bindings(a).contains(&h.binding.as_str())
        })
    };
    let gather = GATHERS.contains(&h.binding.as_str());
    let has = |k: &str| h.text.get(k).is_some_and(|v| !v.is_empty());
    if truth == Some(NOT_EQUIVALENT) && !says(&refutes) && !has("witness") {
        errs.push(if gather {
            "a non-equivalent pair needs a `witness:`".to_string()
        } else {
            "a non-equivalent pair needs `expect fuzz: counterexample` or a `witness:`".to_string()
        });
    }
    if truth == Some(EQUIVALENT) && !says(&|a| evidence_equivalent(a, &h.binding)) && !has("argument") {
        let proved = if h.binding == GATHER_GENERATED { "proved-gather-generated" } else { "proved-gather" };
        errs.push(if gather {
            format!("an equivalent pair needs `expect lean: {proved}` or an `argument:`")
        } else {
            "an equivalent pair needs a prover's `proved` pin, the frontend's `reflexive` or \
             `emit-reflexive`, or an `argument:`"
                .to_string()
        });
    }
    errs
}

// Judgement states. Only OK and KNOWN pass.
/// The pin holds.
pub const OK: &str = "ok";
/// A pinned `!known-unsound` answer, still reproducing.
pub const KNOWN: &str = "known";
/// The answer moved (either way); `--bless` takes it.
pub const CHANGED: &str = "changed";
/// The axis ran and the case has no line for it; `--bless` adds one.
pub const UNPINNED: &str = "unpinned";
/// A `!known-unsound` line whose bug no longer reproduces; `--bless` drops it.
pub const STALE: &str = "stale-marker";
/// The answer contradicts truth and nobody said that is known.
pub const INVARIANT: &str = "invariant";
/// `timeout` / `missing`: there is no answer to pin.
pub const UNANSWERED: &str = "unanswered";
pub const PASSING: [&str; 2] = [OK, KNOWN];
pub const BLESSABLE: [&str; 3] = [CHANGED, UNPINNED, STALE];

#[derive(Clone, Debug)]
pub struct Judgement {
    pub axis: String,
    pub state: &'static str,
    pub observed: String,
    pub pin: Option<Pin>,
    pub note: String,
}

impl Judgement {
    pub fn passed(&self) -> bool {
        PASSING.contains(&self.state)
    }
}

/// Compare what each axis that ran said (`observed`: axis -> (word, note)) with the pins. Axes that
/// did not run are not judged at all: their lines are neither checked nor stale.
pub fn judge(h: &Header, observed: &HashMap<String, (String, String)>) -> Vec<Judgement> {
    let mut out = Vec::new();
    for axis in AXES {
        let Some((word, note)) = observed.get(axis) else { continue };
        let pin = h.pin(axis);
        let state = if UNPINNABLE.contains(&word.as_str()) {
            UNANSWERED
        } else if contradicts(h.truth.as_deref(), axis, word, &h.binding) {
            match pin {
                Some(p) if p.marker => {
                    if &p.word == word {
                        KNOWN
                    } else {
                        CHANGED
                    }
                }
                _ => INVARIANT,
            }
        } else {
            match pin {
                None => UNPINNED,
                Some(p) if p.marker => STALE,
                Some(p) if &p.word != word => CHANGED,
                Some(_) => OK,
            }
        };
        out.push(Judgement {
            axis: axis.to_string(),
            state,
            observed: word.clone(),
            pin: pin.cloned(),
            note: note.clone(),
        });
    }
    out
}

fn rank(axis: &str) -> usize {
    AXES.iter().position(|a| *a == axis).unwrap_or(AXES.len())
}

/// The file with its `expect` lines brought up to date, and nothing else touched.
///
/// Only blessable judgements are written. A line that exists is rewritten in place. A missing one
/// goes in canonical axis order among the `expect` lines already there -- after the last one for an
/// earlier axis, else before the first one for a later axis, else after `truth:` -- so the order
/// does not depend on which axes were blessed first. A marker survives only while its contradiction
/// does, and is never added. Line endings and the final newline are kept, so a second bless is a
/// no-op.
pub fn bless_text(text: &str, h: &Header, judgements: &[Judgement]) -> String {
    let nl = if text.contains("\r\n") { "\r\n" } else { "\n" };
    let mut lines: Vec<String> = splitlines(text).into_iter().map(str::to_string).collect();
    let trailing = text.ends_with('\n') || text.ends_with('\r');
    let mut at_line: Vec<(String, usize)> = h.expect.iter().map(|(a, p)| (a.clone(), p.line)).collect();
    let mut sorted: Vec<&Judgement> = judgements.iter().collect();
    sorted.sort_by_key(|j| rank(&j.axis));
    for j in sorted {
        if !BLESSABLE.contains(&j.state) {
            continue;
        }
        let keep_marker = j.pin.as_ref().is_some_and(|p| p.marker)
            && contradicts(h.truth.as_deref(), &j.axis, &j.observed, &h.binding);
        let new = format!("-- expect {}: {}{}", j.axis, j.observed, if keep_marker { format!(" {MARKER}") } else { String::new() });
        if let Some(p) = &j.pin {
            lines[p.line] = new;
            continue;
        }
        let r = rank(&j.axis);
        let before = at_line.iter().filter(|(a, _)| rank(a) < r).map(|(_, n)| *n).max();
        let after = at_line.iter().filter(|(a, _)| rank(a) > r).map(|(_, n)| *n).min();
        let at = match (before, after) {
            (Some(b), _) => b + 1,
            (None, Some(a)) => a,
            // Python's `h.lines.get("truth", -1) + 1`: with no `truth:` line, the very top.
            (None, None) => h.lines.get("truth").map_or(0, |t| t + 1),
        };
        lines.insert(at, new);
        for (_, n) in at_line.iter_mut() {
            if *n >= at {
                *n += 1;
            }
        }
        at_line.retain(|(a, _)| a != &j.axis);
        at_line.push((j.axis.clone(), at));
    }
    let out = lines.join(nl);
    if trailing {
        out + nl
    } else {
        out
    }
}

/// Replace the file atomically, and only when its bytes would change.
pub fn write_if_changed(path: &Path, text: &str) -> std::io::Result<bool> {
    if std::fs::read(path)? == text.as_bytes() {
        return Ok(false);
    }
    let dir = path.parent().filter(|d| !d.as_os_str().is_empty()).unwrap_or(Path::new("."));
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let (tmp, mut f) = crate::util::create_unique(|suffix| dir.join(format!(".{name}.{suffix}")))?;
    let result = (|| {
        f.write_all(text.as_bytes())?;
        drop(f);
        let mode = std::fs::metadata(path)?.permissions();
        std::fs::set_permissions(&tmp, mode)?;
        std::fs::rename(&tmp, path)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    result.map(|()| true)
}

fn glyph(state: &str) -> &'static str {
    match state {
        OK => "✓",
        KNOWN => "≈",
        CHANGED | STALE => "✗",
        UNPINNED => "+",
        INVARIANT => "‼",
        UNANSWERED => "⏱",
        _ => "?",
    }
}

/// One table cell: what the axis said, and how that compares with the pin.
pub fn cell(j: Option<&Judgement>) -> String {
    let Some(j) = j else { return "·".to_string() };
    let g = glyph(j.state);
    match &j.pin {
        Some(p) if j.state == CHANGED || j.state == STALE => format!("{g} {}→{}", p.word, j.observed),
        _ => format!("{g} {}", j.observed),
    }
}

/// The one-line reason a judgement failed, with what to do about it.
pub fn explain(j: &Judgement, truth: Option<&str>) -> String {
    let pinned = j.pin.as_ref().map_or("None", |p| p.word.as_str());
    let truth = truth.unwrap_or("None");
    let (axis, observed) = (&j.axis, &j.observed);
    match j.state {
        INVARIANT => format!(
            "{axis} says `{observed}` on a pair whose truth is `{truth}` — a soundness failure; --bless will \
             not pin it. Fix the bug, or mark the line {MARKER} and file an issue."
        ),
        UNANSWERED => format!("{axis} gave no answer (`{observed}`); shrink the case or raise --timeout."),
        STALE => format!(
            "{axis}: the {MARKER} answer `{pinned}` no longer reproduces (now `{observed}`); --bless drops the marker."
        ),
        UNPINNED => format!("{axis}: no `expect {axis}:` line; --bless adds `{observed}`."),
        _ => format!("{axis}: pinned `{pinned}`, now `{observed}`; --bless takes the new answer."),
    }
}

#[cfg(test)]
mod tests {
    //! Ported table for table from the Python suite's tests.

    use super::*;

    pub(crate) const LICENCE: &str = "-- Unless explicitly stated otherwise all files in this repository are licensed under the
-- Apache License Version 2.0.
-- This product includes software developed at Datadog (https://www.datadoghq.com/).
-- Copyright 2026-Present Datadog, Inc.
";
    const SQL: &str = "create table \"t\" (\"a\" INTEGER);\nSELECT \"a\" FROM \"t\";\nSELECT \"a\" FROM \"t\" WHERE \"a\" = 1;\n";

    fn pair_sql(directives: &[&str], sql: &str) -> String {
        let mut s = format!("{LICENCE}\n");
        for d in directives {
            s.push_str(d);
            s.push('\n');
        }
        s.push('\n');
        s.push_str(sql);
        s
    }

    fn pair(directives: &[&str]) -> String {
        pair_sql(directives, SQL)
    }

    const NEQ_OK: [&str; 3] = ["-- truth: not-equivalent", "-- origin: a test", "-- witness: t = {(0)}"];
    const EQ_OK: [&str; 3] = ["-- truth: equivalent", "-- origin: a test", "-- argument: because"];

    fn with(base: &[&'static str], more: &[&'static str]) -> Vec<&'static str> {
        base.iter().chain(more).copied().collect()
    }

    fn errors(directives: &[&str]) -> Vec<String> {
        lint(&parse_header(&pair(directives)))
    }

    fn assert_lints(needle: &str, directives: &[&str]) {
        let errs = errors(directives);
        assert!(errs.iter().any(|e| e.contains(needle)), "no {needle:?} in {errs:?} for {directives:?}");
    }

    #[test]
    fn a_complete_header_is_clean() {
        assert_eq!(errors(&with(&NEQ_OK, &["-- expect qed: no-proof"])), Vec::<String>::new());
        assert_eq!(errors(&with(&EQ_OK, &["-- expect fuzz: no-counterexample"])), Vec::<String>::new());
    }

    #[test]
    fn the_licence_and_prose_are_not_directives() {
        let h = parse_header(&pair(&with(&NEQ_OK, &["-- A sentence: with a colon in it."])));
        assert!(h.errors.is_empty(), "{:?}", h.errors);
        let mut keys: Vec<&String> = h.lines.keys().collect();
        keys.sort();
        assert_eq!(keys, ["origin", "truth", "witness"]);
    }

    #[test]
    fn lint_rows() {
        let rows: Vec<(&str, Vec<&str>)> = vec![
            ("no `truth:` line", vec!["-- origin: x", "-- witness: w"]),
            ("is not one of", vec!["-- truth: maybe", "-- origin: x", "-- argument: a"]),
            ("no `origin:` line", vec!["-- truth: equivalent", "-- argument: a"]),
            ("unknown axis `fuz`", with(&NEQ_OK, &["-- expect fuz: counterexample"])),
            ("unknown directive `note:`", with(&NEQ_OK, &["-- note: hello"])),
            ("duplicate `expect qed:`", with(&NEQ_OK, &["-- expect qed: no-proof", "-- expect qed: no-proof"])),
            ("`proven` is not one of", with(&NEQ_OK, &["-- expect qed: proven"])),
            ("never pinnable", with(&NEQ_OK, &["-- expect qed: timeout"])),
            ("takes one word", with(&NEQ_OK, &["-- expect qed: no-proof extra"])),
            ("does not contradict", with(&NEQ_OK, &["-- expect qed: no-proof !known-unsound"])),
            ("does not contradict", with(&EQ_OK, &["-- expect qed: proved !known-unsound"])),
            (
                "needs `expect fuzz: counterexample` or a `witness:`",
                vec!["-- truth: not-equivalent", "-- origin: x", "-- expect fuzz: no-counterexample"],
            ),
            (
                "needs a prover's `proved` pin, the frontend's `reflexive` or `emit-reflexive`, or an `argument:`",
                vec!["-- truth: equivalent", "-- origin: x", "-- expect qed: no-proof"],
            ),
            (
                "needs a prover's `proved` pin, the frontend's `reflexive` or `emit-reflexive`, or an `argument:`",
                // A proof that is itself marked unsound is no evidence for anything.
                vec!["-- truth: equivalent", "-- origin: x", "-- expect fuzz: counterexample"],
            ),
            ("`catalog: guessed` is not one of", with(&EQ_OK, &["-- catalog: guessed"])),
            ("`binding: sideways` is not one of", with(&EQ_OK, &["-- binding: sideways"])),
            // A contradicting pin can only have been written by hand; it is caught without the axis.
            ("contradicts `truth: not-equivalent`", with(&NEQ_OK, &["-- expect qed: proved"])),
            ("contradicts `truth: equivalent`", with(&EQ_OK, &["-- expect fuzz: counterexample"])),
            (
                "contradicts `truth: not-equivalent`",
                with(&NEQ_OK, &["-- binding: gather", "-- expect lean: no-witness"]),
            ),
            // Under the gather rule only Lean's answers are evidence, and `no-witness` is not.
            (
                "a non-equivalent pair needs a `witness:`",
                vec!["-- truth: not-equivalent", "-- binding: gather", "-- origin: x", "-- expect fuzz: counterexample"],
            ),
            (
                "needs `expect lean: proved-gather` or an `argument:`",
                vec!["-- truth: equivalent", "-- binding: gather", "-- origin: x", "-- expect qed: proved"],
            ),
            (
                "needs `expect lean: proved-gather` or an `argument:`",
                vec!["-- truth: equivalent", "-- binding: gather", "-- origin: x", "-- expect lean: no-witness"],
            ),
            // Under `gather` a generated proof claims less than the truth, so it is no evidence...
            (
                "needs `expect lean: proved-gather` or an `argument:`",
                vec![
                    "-- truth: equivalent",
                    "-- binding: gather",
                    "-- origin: x",
                    "-- expect lean: proved-gather-generated",
                ],
            ),
            // ...under `gather-generated` it is, but its possibly vacuous form is not...
            (
                "needs `expect lean: proved-gather-generated` or an `argument:`",
                vec![
                    "-- truth: equivalent",
                    "-- binding: gather-generated",
                    "-- origin: x",
                    "-- expect lean: no-witness-generated",
                ],
            ),
            // ...and either form contradicts a non-equivalent truth.
            (
                "contradicts `truth: not-equivalent`",
                with(&NEQ_OK, &["-- binding: gather-generated", "-- expect lean: no-witness-generated"]),
            ),
            (
                "contradicts `truth: not-equivalent`",
                with(&NEQ_OK, &["-- binding: gather-generated", "-- expect lean: proved-gather"]),
            ),
        ];
        for (needle, directives) in rows {
            assert_lints(needle, &directives);
        }
    }

    #[test]
    fn evidence_from_an_axis_replaces_a_written_one() {
        let none = Vec::<String>::new();
        assert_eq!(errors(&["-- truth: not-equivalent", "-- origin: x", "-- expect fuzz: counterexample"]), none);
        assert_eq!(errors(&["-- truth: equivalent", "-- origin: x", "-- expect qed: proved"]), none);
        assert_eq!(
            errors(&["-- truth: equivalent", "-- binding: gather", "-- origin: x", "-- expect lean: proved-gather"]),
            none
        );
        assert_eq!(
            errors(&[
                "-- truth: equivalent",
                "-- binding: gather-generated",
                "-- origin: x",
                "-- expect lean: proved-gather-generated"
            ]),
            none
        );
    }

    #[test]
    fn an_axis_under_the_other_binding_contradicts_nothing() {
        // The other axes refuse a gather pair, and Lean has nothing to say about an index one.
        let none = Vec::<String>::new();
        assert_eq!(
            errors(&[
                "-- truth: equivalent",
                "-- binding: gather",
                "-- origin: x",
                "-- argument: a",
                "-- expect fuzz: counterexample"
            ]),
            none
        );
        assert_eq!(errors(&with(&NEQ_OK, &["-- expect lean: proved-gather"])), none);
    }

    #[test]
    fn the_frontends_reflexive_answers_are_evidence_on_their_own() {
        for word in FRONTEND_SAME {
            let pin = format!("-- expect frontend: {word}");
            let directives = ["-- truth: equivalent", "-- origin: a test", pin.as_str()];
            assert_eq!(errors(&directives), Vec::<String>::new(), "{word}");
        }
        let refused = ["-- truth: equivalent", "-- origin: a test", "-- expect frontend: refuse:unsupported"];
        assert!(errors(&refused).iter().any(|e| e.contains("an equivalent pair needs")));
    }

    #[test]
    fn a_pin_below_the_sql_is_an_error_not_a_comment() {
        let errs = lint(&parse_header(&pair_sql(&NEQ_OK, &format!("{SQL}-- expect qed: no-proof\n"))));
        assert!(errs.iter().any(|e| e.contains("below the first SQL line")), "{errs:?}");
    }

    #[test]
    fn a_marker_on_a_real_contradiction_is_clean() {
        for directives in [
            with(&NEQ_OK, &["-- expect qed: proved !known-unsound"]),
            with(&NEQ_OK, &["-- expect frontend: emit-reflexive !known-unsound"]),
            with(&NEQ_OK, &["-- expect frontend: reflexive !known-unsound"]),
            with(&EQ_OK, &["-- expect fuzz: counterexample !known-unsound"]),
        ] {
            assert_eq!(errors(&directives), Vec::<String>::new(), "{directives:?}");
        }
    }

    #[test]
    fn the_old_name_of_sqleq_solvers_axis_is_read_as_the_new_one() {
        let h = parse_header(&pair(&with(&NEQ_OK, &["-- expect sqlsolver-rust: no-proof"])));
        assert!(h.errors.is_empty(), "{:?}", h.errors);
        assert!(h.pin("sqleq-solver").is_some());
        assert_eq!(canonical_axis("sqlsolver-rust"), "sqleq-solver");
    }

    #[test]
    fn both_spellings_in_one_header_are_one_axis_twice() {
        let h = parse_header(&pair(&with(
            &NEQ_OK,
            &["-- expect sqleq-solver: no-proof", "-- expect sqlsolver-rust: no-proof"],
        )));
        assert!(h.errors.iter().any(|e| e.contains("second `expect` line for `sqleq-solver`")), "{:?}", h.errors);
    }

    fn obs(axis: &str, word: &str) -> HashMap<String, (String, String)> {
        HashMap::from([(axis.to_string(), (word.to_string(), String::new()))])
    }

    fn judge_one(observed: &HashMap<String, (String, String)>, directives: &[&str]) -> Vec<(String, &'static str)> {
        let h = parse_header(&pair(directives));
        judge(&h, observed).into_iter().map(|j| (j.axis, j.state)).collect()
    }

    /// (label, header directives, (axis, observed word), expected state)
    type JudgeRow<'a> = (&'a str, Vec<&'a str>, (&'a str, &'a str), &'a str);

    #[test]
    fn judge_rows() {
        let rows: Vec<JudgeRow> = vec![
            ("holds", with(&NEQ_OK, &["-- expect qed: no-proof"]), ("qed", "no-proof"), OK),
            // The ratchet: an improvement is a movement too, and fails until blessed.
            ("improvement", with(&EQ_OK, &["-- expect qed: no-proof"]), ("qed", "proved"), CHANGED),
            ("regression", with(&EQ_OK, &["-- expect qed: proved"]), ("qed", "no-proof"), CHANGED),
            ("unpinned", NEQ_OK.to_vec(), ("qed", "no-proof"), UNPINNED),
            ("false proof", NEQ_OK.to_vec(), ("qed", "proved"), INVARIANT),
            ("false proof over a pin", with(&NEQ_OK, &["-- expect qed: no-proof"]), ("qed", "proved-literal"), INVARIANT),
            ("lowered alike", NEQ_OK.to_vec(), ("frontend", "emit-reflexive"), INVARIANT),
            ("refused, but alike", NEQ_OK.to_vec(), ("frontend", "reflexive"), INVARIANT),
            ("a refusal, not alike", NEQ_OK.to_vec(), ("frontend", "refuse:unsupported"), UNPINNED),
            ("false refutation", EQ_OK.to_vec(), ("fuzz", "counterexample"), INVARIANT),
            ("known", with(&NEQ_OK, &["-- expect qed: proved !known-unsound"]), ("qed", "proved"), KNOWN),
            (
                "known, another word",
                with(&NEQ_OK, &["-- expect qed: proved !known-unsound"]),
                ("qed", "proved-literal"),
                CHANGED,
            ),
            ("fixed", with(&NEQ_OK, &["-- expect qed: proved !known-unsound"]), ("qed", "no-proof"), STALE),
            ("timeout", with(&NEQ_OK, &["-- expect qed: no-proof"]), ("qed", "timeout"), UNANSWERED),
            ("missing", NEQ_OK.to_vec(), ("sqlsolver-jvm", "missing"), UNANSWERED),
            ("lean false proof", with(&NEQ_OK, &["-- binding: gather"]), ("lean", "proved-gather"), INVARIANT),
            ("lean vacuous false proof", with(&NEQ_OK, &["-- binding: gather"]), ("lean", "no-witness"), INVARIANT),
            ("lean on an index pair", NEQ_OK.to_vec(), ("lean", "proved-gather"), UNPINNED),
            ("prover on a gather pair", with(&NEQ_OK, &["-- binding: gather"]), ("qed", "proved"), UNPINNED),
            ("fuzz on a gather pair", with(&EQ_OK, &["-- binding: gather"]), ("fuzz", "counterexample"), UNPINNED),
            (
                "lean generated false proof",
                with(&NEQ_OK, &["-- binding: gather-generated"]),
                ("lean", "proved-gather-generated"),
                INVARIANT,
            ),
            (
                "lean generated proof of a gather truth",
                with(&NEQ_OK, &["-- binding: gather"]),
                ("lean", "proved-gather-generated"),
                UNPINNED,
            ),
            (
                "prover on a gather-generated pair",
                with(&NEQ_OK, &["-- binding: gather-generated"]),
                ("qed", "proved"),
                UNPINNED,
            ),
        ];
        for (label, directives, (axis, word), want) in rows {
            assert_eq!(judge_one(&obs(axis, word), &directives), vec![(axis.to_string(), want)], "{label}");
        }
    }

    #[test]
    fn an_axis_that_did_not_run_is_not_judged() {
        let got = judge_one(&obs("fuzz", "counterexample"), &with(&NEQ_OK, &["-- expect qed: proved !known-unsound"]));
        assert_eq!(got, vec![("fuzz".to_string(), UNPINNED)]);
    }

    fn bless(text: &str, observed: &[(&str, &str)]) -> String {
        let h = parse_header(text);
        let observed: HashMap<String, (String, String)> =
            observed.iter().map(|(a, w)| (a.to_string(), (w.to_string(), String::new()))).collect();
        bless_text(text, &h, &judge(&h, &observed))
    }

    #[test]
    fn only_expect_lines_change() {
        let text = pair(&with(&NEQ_OK, &["-- expect qed: proved !known-unsound", "-- expect fuzz: error"]));
        let out = bless(&text, &[("qed", "no-proof"), ("fuzz", "counterexample")]);
        let (old, new) = (splitlines(&text), splitlines(&out));
        assert_eq!(old.len(), new.len());
        let moved: Vec<(&str, &str)> = old.iter().zip(&new).filter(|(a, b)| a != b).map(|(a, b)| (*a, *b)).collect();
        assert_eq!(
            moved,
            [
                ("-- expect qed: proved !known-unsound", "-- expect qed: no-proof"),
                ("-- expect fuzz: error", "-- expect fuzz: counterexample")
            ]
        );
    }

    #[test]
    fn missing_lines_go_in_canonical_order_whatever_ran_first() {
        let observed = [
            ("frontend", "emit"),
            ("fuzz", "counterexample"),
            ("qed", "no-proof"),
            ("sqleq-solver", "no-proof"),
        ];
        let word = |a: &str| observed.iter().find(|(x, _)| *x == a).unwrap().1;
        let mut want: Option<String> = None;
        for order in [vec!["sqleq-solver", "qed"], vec!["qed", "fuzz", "frontend", "sqleq-solver"], vec!["frontend"]] {
            let mut text = pair(&NEQ_OK);
            for axis in order.iter().copied().chain(observed.iter().map(|(a, _)| *a)) {
                text = bless(&text, &[(axis, word(axis))]);
            }
            let w = want.get_or_insert_with(|| text.clone());
            assert_eq!(&text, w, "{order:?}");
        }
        let want = want.unwrap();
        let axes: Vec<&str> = splitlines(&want)
            .into_iter()
            .filter(|l| l.starts_with("-- expect "))
            .map(|l| l.split(':').next().unwrap())
            .collect();
        assert_eq!(axes, ["-- expect frontend", "-- expect fuzz", "-- expect qed", "-- expect sqleq-solver"]);
        assert!(want.find("-- truth:").unwrap() < want.find("-- expect frontend").unwrap());
    }

    #[test]
    fn a_second_bless_is_a_no_op() {
        let observed = [("frontend", "emit"), ("fuzz", "counterexample")];
        let once = bless(&pair(&NEQ_OK), &observed);
        assert_ne!(once, pair(&NEQ_OK));
        assert_eq!(bless(&once, &observed), once);
    }

    #[test]
    fn an_invariant_is_never_pinned_and_a_marker_never_added() {
        let text = pair(&with(&NEQ_OK, &["-- expect qed: no-proof"]));
        assert_eq!(bless(&text, &[("qed", "proved")]), text);
        assert_eq!(bless(&pair(&NEQ_OK), &[("qed", "proved")]), pair(&NEQ_OK));
    }

    #[test]
    fn no_answer_is_never_pinned() {
        let text = pair(&with(&NEQ_OK, &["-- expect qed: no-proof"]));
        assert_eq!(bless(&text, &[("qed", "timeout")]), text);
    }

    #[test]
    fn a_marker_survives_while_its_contradiction_does() {
        let text = pair(&with(&NEQ_OK, &["-- expect qed: proved !known-unsound"]));
        let out = bless(&text, &[("qed", "proved-literal")]);
        assert!(out.contains("-- expect qed: proved-literal !known-unsound\n"), "{out}");
    }

    #[test]
    fn line_endings_and_the_final_newline_are_kept() {
        let crlf = pair(&NEQ_OK).replace('\n', "\r\n");
        let out = bless(&crlf, &[("qed", "no-proof")]);
        assert!(out.contains("-- expect qed: no-proof\r\n"));
        assert!(!out.replace("\r\n", "").contains('\n'), "no bare \\n crept in");
        let bare = pair(&NEQ_OK).trim_end_matches('\n').to_string();
        assert!(!bless(&bare, &[("qed", "no-proof")]).ends_with('\n'));
    }

    #[test]
    fn write_if_changed_does_not_touch_an_unchanged_file() {
        let dir = crate::util::TempDir::new("sqleq-suite-test-").unwrap();
        let p = dir.path().join("a.sql");
        std::fs::write(&p, "x\n").unwrap();
        let before = std::fs::metadata(&p).unwrap().modified().unwrap();
        std::thread::sleep(std::time::Duration::from_millis(20));
        assert!(!write_if_changed(&p, "x\n").unwrap());
        assert_eq!(std::fs::metadata(&p).unwrap().modified().unwrap(), before);
        assert!(write_if_changed(&p, "y\n").unwrap());
        assert_eq!(std::fs::read_to_string(&p).unwrap(), "y\n");
        let left: Vec<_> = std::fs::read_dir(dir.path()).unwrap().collect();
        assert_eq!(left.len(), 1, "the temporary file is renamed over the original, not left behind");
    }

    #[test]
    fn splitlines_matches_python() {
        assert_eq!(splitlines("a\nb\r\nc\rd"), ["a", "b", "c", "d"]);
        assert_eq!(splitlines("a\n"), ["a"]);
        assert_eq!(splitlines("a\n\nb"), ["a", "", "b"]);
        assert_eq!(splitlines(""), Vec::<&str>::new());
        assert_eq!(splitlines("a\u{0c}b"), ["a", "b"]);
    }

    #[test]
    fn directives_follow_the_grammar() {
        assert_eq!(directive("-- expect qed: proved"), Some(("expect qed", "proved")));
        assert_eq!(directive("-- note:"), Some(("note", "")));
        assert_eq!(directive("-- A sentence: x"), None);
        assert_eq!(directive("-- a b c: x"), None);
        assert_eq!(directive("-- a:b"), None);
        assert_eq!(directive("--a: b"), None);
        assert_eq!(directive("-- truth:  equivalent"), Some(("truth", " equivalent")));
    }
}
