// Unless explicitly stated otherwise all files in this repository are licensed under the
// Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/).
// Copyright 2026-Present Datadog, Inc.

//! Every relative link in every Markdown file points at a file that exists.
//!
//! A dangling relative link is the one documentation defect that is both certain to happen --
//! files get renamed, `docs/` gets reorganised -- and invisible to every other check in this repo,
//! because nothing compiles Markdown. It is also the first thing a new reader hits. So it is a
//! gate, not a habit: `cargo test` runs it.
//!
//! Two deliberate choices, both of which have already mattered here:
//!
//! * The file list comes from `git ls-files --cached --others --exclude-standard`, not from
//!   `--cached` alone. A doc that has been written but not yet `git add`ed is exactly the doc whose
//!   links have never been checked, and with `--cached` only, it would be passed over in silence.
//! * HTML `src` / `srcset` / `href` attributes are checked alongside Markdown `[](...)`. The
//!   README's title line is a `<picture>` element holding the logo; moving `docs/logo/` would break
//!   the very first line of the repo's front page without touching a single Markdown link.
//!
//! Fragments (`#section`) are dropped rather than resolved. Checking them would mean deciding what
//! GitHub's heading slugs are, which is a guess about someone else's renderer; checking the path is
//! not.

use std::path::{Component, Path, PathBuf};
use std::process::Command;

const EXTERNAL: [&str; 5] = ["http://", "https://", "mailto:", "//", "#"];

/// Blank out fenced code blocks, keeping line numbers intact so reports stay accurate.
fn strip_fences(src: &str) -> Vec<&str> {
    let mut fenced = false;
    src.split('\n')
        .map(|line| {
            if line.starts_with("```") {
                fenced = !fenced;
                ""
            } else if fenced {
                ""
            } else {
                line
            }
        })
        .collect()
}

/// `[text](target)`, allowing the optional `(target "title")` form. `target` runs to the first
/// whitespace or `)`, which is all this repo's links need -- no `<angle-bracket>` or escaped-paren
/// targets exist here, and a link that grew one would show up as a dangling path rather than pass.
fn md_links(line: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut from = 0;
    while let Some(off) = line[from..].find('[') {
        let start = from + off;
        if let Some((target, end)) = md_link_at(line, start) {
            out.push(target);
            from = end;
        } else {
            from = start + 1;
        }
    }
    out
}

/// The link starting at the `[` at `start`, and the index just past it.
fn md_link_at(line: &str, start: usize) -> Option<(&str, usize)> {
    let close = start + 1 + line[start + 1..].find(']')?;
    let rest = &line[close + 1..];
    let inner = rest.strip_prefix('(')?;
    let t_len = inner.find(|c: char| c == ')' || c.is_whitespace()).unwrap_or(inner.len());
    if t_len == 0 {
        return None;
    }
    let target = &inner[..t_len];
    let mut after = &inner[t_len..];
    if !after.starts_with(')') {
        // `\s+"title"`, then the `)`.
        let trimmed = after.trim_start();
        if trimmed.len() == after.len() {
            return None;
        }
        let title = trimmed.strip_prefix('"')?;
        let q = title.find('"')?;
        after = &title[q + 1..];
        if !after.starts_with(')') {
            return None;
        }
    }
    let end = line.len() - after.len() + 1;
    Some((target, end))
}

/// `(?:src|srcset|href)="([^"]+)"`.
fn html_attrs(line: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < line.len() {
        if !line.is_char_boundary(i) {
            i += 1;
            continue;
        }
        let hit = ["src=\"", "srcset=\"", "href=\""].iter().find_map(|p| {
            let rest = line[i..].strip_prefix(p)?;
            let q = rest.find('"').filter(|q| *q > 0)?;
            Some((&rest[..q], i + p.len() + q + 1))
        });
        match hit {
            Some((t, end)) => {
                out.push(t);
                i = end;
            }
            None => i += 1,
        }
    }
    out
}

/// `(line number, target)` for every link worth resolving.
fn targets(src: &str) -> Vec<(usize, &str)> {
    let mut out = Vec::new();
    for (n, line) in strip_fences(src).into_iter().enumerate() {
        for t in md_links(line).into_iter().chain(html_attrs(line)) {
            if !EXTERNAL.iter().any(|e| t.starts_with(e)) {
                out.push((n + 1, t));
            }
        }
    }
    out
}

/// `os.path.normpath`: lexical, so `a/../b` is `b` even when `a` is a symlink.
fn normpath(p: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for c in p.components() {
        match c {
            Component::CurDir => {}
            Component::ParentDir => {
                if !out.pop() {
                    out.push("..");
                }
            }
            c => out.push(c.as_os_str()),
        }
    }
    out
}

fn git(root: &Path, args: &[&str]) -> String {
    let out = Command::new("git").args(args).current_dir(root).output().expect("git runs");
    assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8(out.stdout).unwrap()
}

#[test]
fn every_relative_link_resolves() {
    let here = Path::new(env!("CARGO_MANIFEST_DIR"));
    let root = PathBuf::from(git(here, &["rev-parse", "--show-toplevel"]).trim());
    let listing = git(&root, &["ls-files", "--cached", "--others", "--exclude-standard"]);
    let files: Vec<&str> = listing.lines().filter(|f| f.ends_with(".md")).collect();
    let mut bad = Vec::new();
    for f in &files {
        let Ok(src) = std::fs::read_to_string(root.join(f)) else { continue };
        let dir = Path::new(f).parent().unwrap_or(Path::new(""));
        for (line, t) in targets(&src) {
            let path = t.split('#').next().unwrap_or("");
            if path.is_empty() {
                continue;
            }
            if !root.join(normpath(&dir.join(path))).exists() {
                bad.push(format!("{f}:{line}: dangling link -> {t}"));
            }
        }
    }
    assert!(bad.is_empty(), "{} markdown files checked, {} dangling link(s):\n{}", files.len(), bad.len(), bad.join("\n"));
    assert!(!files.is_empty());
}

#[test]
fn the_link_rules_are_the_ones_they_say() {
    assert_eq!(md_links("see [a](b.md) and [c](d.md \"title\")"), ["b.md", "d.md"]);
    assert_eq!(md_links("[a [b](c)"), ["c"]);
    assert_eq!(md_links("[x](no close"), Vec::<&str>::new());
    assert_eq!(md_links("[x]( y)"), Vec::<&str>::new());
    assert_eq!(html_attrs(r#"<img src="a.svg" srcset="b.svg"> <a href="c.md">"#), ["a.svg", "b.svg", "c.md"]);
    assert_eq!(html_attrs(r#"src="""#), Vec::<&str>::new());
    assert_eq!(targets("[a](http://x) [b](#frag) [c](./d.md#e)\n```\n[z](gone.md)\n```\n"), [(1, "./d.md#e")]);
    assert_eq!(normpath(Path::new("docs/../README.md")), PathBuf::from("README.md"));
}
