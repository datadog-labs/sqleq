// Unless explicitly stated otherwise all files in this repository are licensed under the
// Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/).
// Copyright 2026-Present Datadog, Inc.

//! Which files are cases, what each is called, and whether a pair is `x` against `x`.
//!
//! Many pairs reach the prover as `x` against `x`: the preprocessor's normalizations are themselves
//! equivalence-preserving rewrites, and on some pairs the rewrite they undo *is* the optimization
//! under test. The end-to-end claim stays sound -- normalize soundly, then prove -- but the credit
//! does not belong to the prover, so a raw "proved N/M" can overstate capability badly. Every
//! report therefore carries the split.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use serde_json::Value;

/// Split on top-level `;`, dropping declarations. Quote- and comment-aware, because a `;` inside a
/// string literal or a `--` comment does not end a statement and mis-splitting would silently
/// mis-classify the case.
pub fn statements(sql: &str) -> Vec<String> {
    let s: Vec<char> = sql.chars().collect();
    let starts = |i: usize, pat: &str| pat.chars().enumerate().all(|(k, c)| s.get(i + k) == Some(&c));
    let find = |from: usize, pat: &str| (from..s.len()).find(|&j| starts(j, pat));
    let (mut out, mut buf, mut quote, mut i) = (Vec::new(), String::new(), None::<char>, 0);
    while i < s.len() {
        let ch = s[i];
        if let Some(q) = quote {
            buf.push(ch);
            if ch == q {
                // A doubled quote is an escaped quote, not a terminator.
                if s.get(i + 1) == Some(&q) {
                    buf.push(q);
                    i += 2;
                    continue;
                }
                quote = None;
            }
            i += 1;
            continue;
        }
        if ch == '\'' || ch == '"' {
            quote = Some(ch);
            buf.push(ch);
        } else if starts(i, "--") {
            i = find(i, "\n").unwrap_or(s.len());
            continue;
        } else if starts(i, "/*") {
            i = find(i + 2, "*/").map_or(s.len(), |e| e + 2);
            continue;
        } else if ch == ';' {
            out.push(std::mem::take(&mut buf));
        } else {
            buf.push(ch);
        }
        i += 1;
    }
    out.push(buf);

    out.into_iter()
        .map(|st| st.split_whitespace().collect::<Vec<_>>().join(" "))
        .filter(|st| {
            let head = st.to_lowercase();
            !st.is_empty() && !["create ", "declare ", "drop ", "set ", "insert "].iter().any(|p| head.starts_with(p))
        })
        .collect()
}

/// True when the two queries are textually identical after whitespace normalization. Weaker than
/// the IR test -- it misses alias-only and quoting differences -- and used only when there is no IR
/// to look at.
pub fn triviality_from_text(sql: &str) -> Option<bool> {
    let qs = statements(sql);
    (qs.len() == 2).then(|| qs[0] == qs[1])
}

/// True when the two lowered query plans are structurally equal, i.e. the prover was handed `x`
/// against `x` and equivalence is reflexivity. This is the definition that matters: it is exactly
/// what the prover sees, so it also catches pairs that differ only in aliases, quoting or
/// whitespace.
pub fn triviality_from_ir(plan: &Value) -> Option<bool> {
    match plan.get("queries") {
        Some(Value::Array(qs)) if qs.len() == 2 => Some(qs[0] == qs[1]),
        _ => None,
    }
}

const INPUT_EXTS: [&str; 2] = [".sql", ".json"];

/// The extension as Python's `Path.suffix` spells it: `.sql`, or empty.
pub fn suffix(p: &Path) -> String {
    p.extension().map(|e| format!(".{}", e.to_string_lossy())).unwrap_or_default()
}

fn walk(dir: &Path, ext: &str, out: &mut Vec<PathBuf>) {
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    let mut entries: Vec<PathBuf> = rd.flatten().map(|e| e.path()).collect();
    entries.sort();
    for p in entries {
        // Symlinked directories are not followed, as `Path.rglob` does not follow them.
        let is_link_dir = p.symlink_metadata().is_ok_and(|m| m.file_type().is_symlink()) && p.is_dir();
        if p.is_dir() && !is_link_dir {
            walk(&p, ext, out);
        } else if p.is_file() && p.file_name().is_some_and(|n| n.to_string_lossy().ends_with(ext)) {
            out.push(p);
        }
    }
}

/// Collect `.sql` and `.json` inputs. When a `.sql` and a `.json` share the same directory and
/// stem (i.e. the `.json` was generated from that `.sql`), keep only the `.sql` so the same case is
/// not run twice. Returns the files and a warning per argument that was skipped.
pub fn collect_inputs(paths: &[String]) -> (Vec<PathBuf>, Vec<String>) {
    let mut found: Vec<PathBuf> = Vec::new();
    let mut seen: HashSet<PathBuf> = HashSet::new();
    let mut warnings = Vec::new();
    let resolve = |p: &Path| p.canonicalize().unwrap_or_else(|_| crate::util::abspath(p));
    for p in paths {
        let pp = Path::new(p);
        if pp.is_dir() {
            for ext in INPUT_EXTS {
                let mut fs = Vec::new();
                walk(pp, ext, &mut fs);
                for f in fs {
                    if seen.insert(resolve(&f)) {
                        found.push(f);
                    }
                }
            }
        } else if pp.is_file() && INPUT_EXTS.contains(&suffix(pp).as_str()) {
            if seen.insert(resolve(pp)) {
                found.push(pp.to_path_buf());
            }
        } else {
            warnings.push(format!("warning: skipping unsupported input: {p}"));
        }
    }
    let key = |f: &Path| (resolve(f).parent().map(Path::to_path_buf), f.file_stem().map(|s| s.to_os_string()));
    let sql_stems: HashSet<_> = found.iter().filter(|f| suffix(f) == ".sql").map(|f| key(f)).collect();
    let mut deduped: Vec<PathBuf> =
        found.into_iter().filter(|f| !(suffix(f) == ".json" && sql_stems.contains(&key(f)))).collect();
    deduped.sort_by_key(|f| f.to_string_lossy().to_lowercase());
    (deduped, warnings)
}

/// The deepest directory (or file) every input lies under.
pub fn common_root(files: &[PathBuf]) -> PathBuf {
    let resolved: Vec<PathBuf> =
        files.iter().map(|f| f.canonicalize().unwrap_or_else(|_| crate::util::abspath(f))).collect();
    let Some(first) = resolved.first() else { return PathBuf::from(".") };
    let mut root: Vec<_> = first.components().collect();
    for p in &resolved[1..] {
        let n = root.iter().zip(p.components()).take_while(|(a, b)| **a == *b).count();
        root.truncate(n);
    }
    root.iter().collect()
}

/// A case's display name: its path relative to the common root, or its file name when it *is* the
/// root (a single input).
pub fn display_name(f: &Path, root: &Path) -> String {
    let abs = f.canonicalize().unwrap_or_else(|_| crate::util::abspath(f));
    match abs.strip_prefix(root) {
        Ok(rel) if !rel.as_os_str().is_empty() => rel.to_string_lossy().into_owned(),
        _ => f.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn declarations_are_dropped() {
        let sql = "create table \"t\" (\"a\" INTEGER);\n\
                   declare scalar function f(INTEGER) returns INTEGER;\n\
                   select \"a\" from \"t\";\nselect \"a\" from \"t\";";
        assert_eq!(statements(sql), ["select \"a\" from \"t\"", "select \"a\" from \"t\""]);
    }

    #[test]
    fn whitespace_is_normalized() {
        assert_eq!(statements("select   1\n  +\t2; select 1 + 2;"), ["select 1 + 2", "select 1 + 2"]);
    }

    #[test]
    fn semicolon_inside_a_string_literal_does_not_split() {
        assert_eq!(statements("select ';' ; select ';';"), ["select ';'", "select ';'"]);
    }

    #[test]
    fn semicolon_inside_a_quoted_identifier_does_not_split() {
        assert_eq!(statements("select \"a;b\" from \"t\"; select 1;"), ["select \"a;b\" from \"t\"", "select 1"]);
    }

    #[test]
    fn doubled_quote_is_an_escape_not_a_terminator() {
        // If the doubled quote ended the literal, the `;` after it would split.
        assert_eq!(statements("select 'it''s; fine'; select 2;"), ["select 'it''s; fine'", "select 2"]);
    }

    #[test]
    fn line_comment_hides_a_semicolon() {
        assert_eq!(statements("select 1 -- ; not a split\n; select 1;"), ["select 1", "select 1"]);
    }

    #[test]
    fn block_comment_hides_a_semicolon() {
        assert_eq!(statements("select /* ; */ 1; select 1;"), ["select 1", "select 1"]);
    }

    #[test]
    fn unterminated_comment_swallows_the_rest() {
        // Degenerate input; the point is that it terminates and yields a count other than two, so
        // the case is reported undetermined rather than guessed at.
        assert_eq!(triviality_from_text("select 1; /* unterminated"), None);
    }

    #[test]
    fn identical_modulo_whitespace() {
        let sql = "create table \"t\" (\"a\" INTEGER);\nselect  \"a\"  from \"t\";\nselect \"a\" from \"t\";";
        assert_eq!(triviality_from_text(sql), Some(true));
    }

    #[test]
    fn different() {
        assert_eq!(triviality_from_text("select \"a\" from \"t\"; select \"b\" from \"t\";"), Some(false));
    }

    #[test]
    fn wrong_statement_count_is_undetermined() {
        assert_eq!(triviality_from_text("select 1;"), None);
        assert_eq!(triviality_from_text("select 1; select 2; select 3;"), None);
    }

    #[test]
    fn the_text_test_is_blind_to_aliasing() {
        // Both sides mean the same thing and lower to the same plan, but the text differs. This is
        // exactly why the IR test is preferred when there is an IR to look at.
        assert_eq!(triviality_from_text("select \"a\" as \"x\" from \"t\"; select \"a\" as \"y\" from \"t\";"), Some(false));
    }

    #[test]
    fn structurally_equal_plans() {
        assert_eq!(triviality_from_ir(&json!({"queries": [{"scan": 0}, {"scan": 0}], "schemas": []})), Some(true));
    }

    #[test]
    fn structurally_different_plans() {
        assert_eq!(triviality_from_ir(&json!({"queries": [{"scan": 0}, {"scan": 1}], "schemas": []})), Some(false));
    }

    #[test]
    fn key_order_does_not_matter() {
        // The plan is a tree, not a serialization.
        let a = json!({"project": {"cols": [0, 1], "input": {"scan": 0}}});
        let b = json!({"project": {"input": {"scan": 0}, "cols": [0, 1]}});
        assert_eq!(triviality_from_ir(&json!({"queries": [a, b]})), Some(true));
    }

    #[test]
    fn list_order_does_matter() {
        let a = json!({"project": {"cols": [0, 1], "input": {"scan": 0}}});
        let b = json!({"project": {"cols": [1, 0], "input": {"scan": 0}}});
        assert_eq!(triviality_from_ir(&json!({"queries": [a, b]})), Some(false));
    }

    #[test]
    fn malformed_plans_are_undetermined() {
        assert_eq!(triviality_from_ir(&json!({})), None);
        assert_eq!(triviality_from_ir(&json!({"queries": [{"scan": 0}]})), None);
        assert_eq!(triviality_from_ir(&json!({"queries": "not a list"})), None);
    }

    #[test]
    fn a_json_beside_its_sql_is_the_same_case() {
        let d = crate::util::TempDir::new("sqleq-inputs-test-").unwrap();
        for f in ["a.sql", "a.json", "b.json", "sub/C.sql", "notes.txt"] {
            let p = d.path().join(f);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, "").unwrap();
        }
        let (files, warnings) = collect_inputs(&[d.path().to_string_lossy().into_owned()]);
        let names: Vec<String> = files.iter().map(|f| display_name(f, &common_root(&files))).collect();
        assert_eq!(names, ["a.sql", "b.json", "sub/C.sql"]);
        assert!(warnings.is_empty());
        let one = vec![d.path().join("a.sql")];
        assert_eq!(display_name(&one[0], &common_root(&one)), "a.sql");
        let (_, w) = collect_inputs(&[d.path().join("notes.txt").to_string_lossy().into_owned()]);
        assert_eq!(w.len(), 1);
    }
}
