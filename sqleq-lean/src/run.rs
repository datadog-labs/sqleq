// Unless explicitly stated otherwise all files in this repository are licensed under the
// Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/).
// Copyright 2026-Present Datadog, Inc.

//! Run Lean on a batch file and read off which pairs the kernel accepted.
//!
//! A pair counts as proved only if **all** of these hold:
//! - Lean reported no error inside the pair's lines;
//! - `#print axioms` printed a line for its `equiv`;
//! - every axiom on that line is in [`ALLOWED`].
//!
//! The axiom check is the trust boundary, not the exit code. A theorem whose proof failed to
//! elaborate is still added to the environment with `sorryAx`, and `native_decide` would add
//! `Lean.ofReduceBool`; both fall outside the allow-list.

use std::collections::HashMap;
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// Lean's three standard axioms. Anything else means the kernel did not check the proof on its own.
pub const ALLOWED: [&str; 3] = ["propext", "Classical.choice", "Quot.sound"];

/// Where `lake` and the `lean/` package are.
#[derive(Clone, Debug)]
pub struct Lean {
    pub lake: PathBuf,
    pub package: PathBuf,
    pub timeout: Duration,
}

impl Lean {
    /// `$LAKE` or `lake` on `PATH`; `$SQLEQ_LEAN_DIR` or the repository's `lean/` package.
    pub fn from_env(timeout: Duration) -> Lean {
        let lake = std::env::var_os("LAKE").map(PathBuf::from).unwrap_or_else(|| "lake".into());
        let package = std::env::var_os("SQLEQ_LEAN_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| Path::new(env!("CARGO_MANIFEST_DIR")).join("../lean"));
        Lean { lake, package, timeout }
    }

    /// Build the library once, so every batch imports compiled `.olean`s.
    pub fn build(&self) -> Result<(), String> {
        let out = Command::new(&self.lake)
            .arg("build")
            .arg("Sqleq")
            .current_dir(&self.package)
            .output()
            .map_err(|e| format!("cannot run {}: {e}", self.lake.display()))?;
        if out.status.success() {
            Ok(())
        } else {
            Err(format!(
                "lake build failed:\n{}{}",
                String::from_utf8_lossy(&out.stdout),
                String::from_utf8_lossy(&out.stderr)
            ))
        }
    }

    /// Check one batch file. Returns Lean's combined output, or `None` on timeout.
    pub fn check(&self, file: &Path) -> Result<Option<String>, String> {
        let mut child = Command::new(&self.lake)
            .args(["env", "lean"])
            .arg(file)
            .current_dir(&self.package)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| format!("cannot run {}: {e}", self.lake.display()))?;
        let start = Instant::now();
        // Drain both pipes on threads so a large output cannot block the child.
        let mut so = child.stdout.take().unwrap();
        let mut se = child.stderr.take().unwrap();
        let t_out = std::thread::spawn(move || {
            let mut s = String::new();
            let _ = std::io::Read::read_to_string(&mut so, &mut s);
            s
        });
        let t_err = std::thread::spawn(move || {
            let mut s = String::new();
            let _ = std::io::Read::read_to_string(&mut se, &mut s);
            s
        });
        loop {
            if child.try_wait().map_err(|e| e.to_string())?.is_some() {
                break;
            }
            if start.elapsed() > self.timeout {
                let _ = child.kill();
                let _ = child.wait();
                return Ok(None);
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        Ok(Some(t_out.join().unwrap_or_default() + &t_err.join().unwrap_or_default()))
    }
}

/// What the kernel said about one entry of a batch.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Outcome {
    Proved,
    /// Not proved; the first Lean error in the entry, or why the axiom check failed.
    Failed(String),
}

/// Read Lean's output for a batch. Each entry is the 0-based line range it occupies and the
/// theorem whose axiom report stands for it.
pub fn outcomes(output: &str, entries: &[(Range<usize>, String)]) -> Vec<Outcome> {
    // `<file>:<line>:<col>: error: <message>`, lines 1-based.
    let mut errors: Vec<(usize, String)> = Vec::new();
    let mut axioms: HashMap<String, Vec<String>> = HashMap::new();
    for l in output.lines() {
        if let Some((head, msg)) = l.split_once(": error") {
            let mut parts = head.rsplitn(3, ':');
            let _col = parts.next();
            if let Some(line) = parts.next().and_then(|s| s.parse::<usize>().ok()) {
                errors.push((line - 1, msg.trim_start_matches(':').trim().to_string()));
                continue;
            }
        }
        if let Some((name, rest)) = l.strip_prefix('\'').and_then(|r| r.split_once('\'')) {
            if let Some(list) = rest.trim().strip_prefix("depends on axioms: [") {
                let list = list.trim_end_matches(']');
                axioms.insert(
                    name.to_string(),
                    list.split(',').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect(),
                );
            } else if rest.contains("does not depend on any axioms") {
                axioms.insert(name.to_string(), Vec::new());
            }
        }
    }
    entries
        .iter()
        .map(|(r, theorem)| {
            if let Some((_, msg)) = errors.iter().find(|(line, _)| r.contains(line)) {
                return Outcome::Failed(format!("lean: {msg}"));
            }
            match axioms.get(theorem) {
                None => Outcome::Failed("no axiom report for the theorem".into()),
                Some(ax) => match ax.iter().find(|a| !ALLOWED.contains(&a.as_str())) {
                    Some(bad) => Outcome::Failed(format!("proof depends on {bad}")),
                    None => Outcome::Proved,
                },
            }
        })
        .collect()
}

/// Read the `(tag, WResult.…)` lines [`crate::emit::reasons`] makes Lean print.
pub fn reduced(output: &str) -> HashMap<usize, String> {
    output
        .lines()
        .filter_map(|l| {
            let inner = l.trim().strip_prefix('(')?.strip_suffix(')')?;
            let (tag, rest) = inner.split_once(", ")?;
            let rest = rest.trim();
            let rest = rest.strip_prefix("Sqleq.").unwrap_or(rest);
            Some((tag.parse().ok()?, rest.strip_prefix("WResult.")?.to_string()))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reduce_lines_are_read_by_tag() {
        let r = reduced("(3, WResult.unique 0)\n(12, Sqleq.WResult.notNull 4)\nnoise\n");
        assert_eq!(r[&3], "unique 0");
        assert_eq!(r[&12], "notNull 4");
        assert_eq!(r.len(), 2);
    }

    #[test]
    fn axioms_and_errors_are_charged_to_the_right_entry() {
        let out = "\
'Q0.equiv' depends on axioms: [propext, Quot.sound]
/tmp/b.lean:14:4: error: Tactic `decide` proved that the proposition
'Q1.equiv' depends on axioms: [propext, sorryAx]
'Q2.equiv' depends on axioms: [propext, Lean.ofReduceBool]
'Q3.equiv' does not depend on any axioms
";
        let ranges: Vec<(Range<usize>, String)> = [3..12, 12..21, 21..30, 30..39, 39..48]
            .into_iter()
            .enumerate()
            .map(|(i, r)| (r, format!("Q{i}.equiv")))
            .collect();
        let got = outcomes(out, &ranges);
        assert_eq!(got[0], Outcome::Proved);
        assert!(matches!(&got[1], Outcome::Failed(m) if m.starts_with("lean:")));
        assert!(matches!(&got[2], Outcome::Failed(m) if m.contains("Lean.ofReduceBool")));
        assert_eq!(got[3], Outcome::Proved);
        assert!(matches!(&got[4], Outcome::Failed(m) if m.contains("no axiom report")));
    }
}
