// Unless explicitly stated otherwise all files in this repository are licensed under the
// Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/).
// Copyright 2026-Present Datadog, Inc.

//! Hygiene over the committed cases: each one lints, carries the licence, and names nothing that
//! belongs to a corpus or a machine.

use std::path::{Path, PathBuf};

use sqleq_check::discover::repo;
use sqleq_check::suite;

fn sql_files(dir: &Path, recurse: bool, out: &mut Vec<PathBuf>) {
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    for e in rd.flatten() {
        let p = e.path();
        if p.is_dir() && recurse {
            sql_files(&p, true, out);
        } else if p.extension().is_some_and(|x| x == "sql") {
            out.push(p);
        }
    }
}

fn files() -> Vec<PathBuf> {
    let mut pairs = Vec::new();
    sql_files(&repo().join("tests").join("pairs"), true, &mut pairs);
    pairs.sort();
    let mut examples = Vec::new();
    sql_files(&repo().join("examples"), false, &mut examples);
    examples.sort();
    pairs.into_iter().chain(examples).collect()
}

fn rel(f: &Path) -> String {
    f.strip_prefix(repo()).unwrap_or(f).display().to_string()
}

#[test]
fn there_are_cases() {
    assert!(files().len() >= 10);
}

#[test]
fn each_case_lints_clean() {
    let bad: Vec<String> = files()
        .iter()
        .filter_map(|f| {
            let errs = suite::lint(&suite::parse_header(&std::fs::read_to_string(f).unwrap()));
            (!errs.is_empty()).then(|| format!("{}: {errs:?}", rel(f)))
        })
        .collect();
    assert!(bad.is_empty(), "{bad:#?}");
}

#[test]
fn each_case_carries_the_licence() {
    for f in files() {
        let text = std::fs::read_to_string(&f).unwrap();
        let head: String = text.split_inclusive('\n').take(6).collect();
        assert!(head.contains("Apache License Version 2.0"), "{}", rel(&f));
    }
}

/// `\bpair\d{2,}\b`: a corpus row's name.
fn names_a_row(text: &str) -> bool {
    let word = |c: Option<char>| c.is_some_and(|c| c.is_alphanumeric() || c == '_');
    text.match_indices("pair").any(|(i, _)| {
        let before = text[..i].chars().next_back();
        let rest = &text[i + 4..];
        let digits = rest.chars().take_while(char::is_ascii_digit).count();
        !word(before) && digits >= 2 && !word(rest[digits..].chars().next())
    })
}

#[test]
fn no_case_names_a_corpus_row_or_a_machine() {
    for f in files() {
        let text = std::fs::read_to_string(&f).unwrap();
        let machine = ["/home/", "/Users/"].iter().any(|p| text.contains(p));
        assert!(!names_a_row(&text) && !machine, "{}", rel(&f));
    }
}

#[test]
fn the_row_name_pattern_is_the_one_it_says() {
    // Assembled here, so this file does not itself carry a row-shaped name.
    assert!(names_a_row(&format!("from pair{} here", 1234)));
    assert!(!names_a_row("pair1"));
    assert!(!names_a_row("pairs12"));
    assert!(!names_a_row("repair12"));
    assert!(!names_a_row("pair12x"));
}
