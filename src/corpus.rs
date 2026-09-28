// Unless explicitly stated otherwise all files in this repository are licensed under the
// Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/).
// Copyright 2026-Present Datadog, Inc.

//! Read a corpus CSV and lower each row straight to prover `Input` JSON.
//!
//! This is the batch driver. Lowering the whole file in one pass collapses what would otherwise be
//! a two-step pipeline (CSV → one `.sql` file per row → frontend → JSON) into one, and keeps the
//! `.sql` intermediate off the shipping path: it stays a convenient hand-written input format
//! rather than a stage every row has to survive.
//!
//! A row is three fields: query A, query B, and the raw Postgres DDL both were run against. Only the
//! first two are required; a row without DDL has no declared schema, and lowering it means either
//! inference or a refusal (see [`CatalogSource`]).
//!
//! Row *n* of the CSV is case `pair{n:04}`. The name is derived from the row number and nothing
//! else, so two reports over the same file can be diffed case for case.

use std::path::Path;

use serde_json::{json, Value};

use crate::error::{schema, FrontendError};
use crate::{lower_with, lower_with_ddl, CatalogSource, Result};

/// One corpus row: the pair, and the DDL it was collected against.
pub struct Row {
    /// 0-based row index in the CSV, which is also the case's number.
    pub index: usize,
    pub a: String,
    pub b: String,
    /// `None` when the row has no DDL field, or it is blank.
    pub ddl: Option<String>,
}

impl Row {
    /// The case name: `pair` followed by the row index, zero-padded to four digits.
    pub fn name(&self) -> String {
        format!("pair{:04}", self.index)
    }
}

/// Read the corpus. The CSV has no header line, and its DDL field holds quoted multi-line SQL, so
/// this needs a real CSV reader rather than a split on commas.
///
/// Rows with fewer than two fields are dropped, as the preprocessor drops them — with no second
/// query there is no pair. Their indices are still consumed, so case numbering does not shift.
pub fn read(path: &Path) -> std::result::Result<Vec<Row>, String> {
    let mut rdr = csv::ReaderBuilder::new()
        .has_headers(false)
        .flexible(true)
        .from_path(path)
        .map_err(|e| format!("cannot read {}: {e}", path.display()))?;
    let mut out = Vec::new();
    for (index, rec) in rdr.records().enumerate() {
        let rec = rec.map_err(|e| format!("{}: row {index}: {e}", path.display()))?;
        if rec.len() < 2 {
            continue;
        }
        let ddl = rec.get(2).filter(|s| !s.trim().is_empty()).map(str::to_string);
        out.push(Row { index, a: rec[0].to_string(), b: rec[1].to_string(), ddl });
    }
    Ok(out)
}

/// Lower one row to the prover's `Input` JSON.
///
/// The two queries are concatenated into the format [`lower_with`] already speaks. That is a join on
/// `;`, not a parse — which is safe here only because the resulting text is then parsed as a whole
/// and the input parser counts the `SELECT`s it ends up with, refusing anything but exactly two. A
/// stray `;` inside one of the queries therefore cannot smuggle in a third statement unnoticed.
pub fn lower(row: &Row, source: CatalogSource) -> Result<Value> {
    let src = src_of(row);
    match &row.ddl {
        Some(d) => lower_with_ddl(&src, d, source),
        // No DDL and no `CREATE TABLE`s in the queries: `Declared` has nothing to lower against and
        // says so, which is the honest answer rather than a silent guess.
        None if source == CatalogSource::Declared => {
            Err(schema("row has no DDL and no CREATE TABLE"))
        }
        None => lower_with(&src, source),
    }
}

/// The pair as the `.sql` format spells it. Shared by [`lower`] and [`reflexive`] so the two can
/// never end up reading different text for the same row.
fn src_of(row: &Row) -> String {
    format!("{};\n{};", row.a.trim().trim_end_matches(';'), row.b.trim().trim_end_matches(';'))
}

/// Whether the row's two sides normalize to the same tree — see [`crate::reflexive`].
///
/// The row's DDL is not passed and is not needed: the check runs only the normalizations, and none
/// of them takes a catalog. So this answers for rows whose schema is absent, incomplete, or simply
/// unreadable by us, which is most of the population it exists to reach.
pub fn reflexive(row: &Row) -> bool {
    crate::reflexive(&src_of(row))
}

/// [`reflexive`] with an explicit rewrite set, for attributing a verdict — see [`crate::Rewrites`].
pub fn reflexive_with(row: &Row, rewrites: crate::Rewrites) -> bool {
    crate::reflexive_with(&src_of(row), rewrites)
}

/// [`crate::reflexive_forms`] for a corpus row.
pub fn reflexive_forms(row: &Row, rewrites: crate::Rewrites) -> Option<(String, String)> {
    crate::reflexive_forms(&src_of(row), rewrites)
}

/// The bucket a refusal falls into, for the report's histogram.
pub fn kind(e: &FrontendError) -> &'static str {
    match e {
        FrontendError::Parse(_) => "parse",
        FrontendError::Unsupported(_) => "unsupported",
        FrontendError::Schema(_) => "schema",
        FrontendError::ParameterMisaligned(_) => "parameter-misaligned",
    }
}

/// One row's outcome, as it appears in the report.
///
/// `reflexive` is [`reflexive`]'s answer, and only ever `true` alongside a refusal — a row that
/// lowers is already reported by its IR, and the harness labels an identical pair of those `trivial`.
/// It becomes a third `status` rather than a flag on `refuse` because it is a *decided* row and the
/// consumers count statuses.
///
/// The refusal's `kind` and `reason` stay on the record anyway. That is deliberate: what would have
/// blocked the row is exactly the datum the coverage roadmap is ranked on, and a reflexive row is
/// evidence that its family's count was inflated by first-failure masking. Dropping the reason here
/// would destroy the measurement this status exists to correct.
pub fn record(row: &Row, outcome: &Result<Value>, reflexive: bool) -> Value {
    match outcome {
        Ok(_) => json!({ "row": row.index, "name": row.name(), "status": "emit" }),
        Err(e) => json!({
            "row": row.index,
            "name": row.name(),
            "status": if reflexive { "reflexive" } else { "refuse" },
            "kind": kind(e),
            "reason": e.to_string(),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(a: &str, b: &str, ddl: Option<&str>) -> Row {
        Row { index: 7, a: a.into(), b: b.into(), ddl: ddl.map(str::to_string) }
    }

    /// The case number is the CSV row number, zero-padded to four, so that two reports over the
    /// same file line up case for case.
    #[test]
    fn case_name_is_the_zero_padded_row_number() {
        let named = |index| Row { index, a: String::new(), b: String::new(), ddl: None }.name();
        assert_eq!(row("", "", None).name(), "pair0007"); // scrub-ok: an index this test invents
        assert_eq!(named(936), "pair0936"); // scrub-ok: an index this test invents
        assert_eq!(named(12345), "pair12345"); // scrub-ok: an index this test invents
    }

    fn write(body: &str) -> std::path::PathBuf {
        let path = std::env::temp_dir().join(format!("sqleq-corpus-{}.csv", body.len()));
        std::fs::write(&path, body).unwrap();
        path
    }

    /// The DDL field holds quoted, multi-line SQL with embedded commas. Reading it needs a real CSV
    /// parser; a split on `,` would shred it.
    #[test]
    fn reads_quoted_multiline_ddl() {
        let path = write("select 1,select 2,\"CREATE TABLE t (\n  a int,\n  b int\n);\"\n");
        let rows = read(&path).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].a, "select 1");
        assert_eq!(rows[0].ddl.as_deref(), Some("CREATE TABLE t (\n  a int,\n  b int\n);"));
    }

    /// A row with no second query is not a pair, so it is dropped — but the rows after it keep the
    /// numbers they would have had, or every case name downstream of a short row would shift.
    #[test]
    fn short_rows_drop_without_renumbering_the_rest() {
        let path = write("oops\nselect 1,select 2\nselect 3,select 4\n");
        let rows = read(&path).unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].name(), "pair0001"); // scrub-ok: an index this test invents
        assert_eq!(rows[1].name(), "pair0002"); // scrub-ok: an index this test invents
    }

    /// A blank third field is no DDL, not empty DDL: an empty catalog would refuse with "unknown
    /// table" instead of saying the row never had a schema.
    #[test]
    fn blank_ddl_field_is_none() {
        let path = write("select 1,select 2,  \n");
        assert!(read(&path).unwrap()[0].ddl.is_none());
    }

    /// The pair reaches the parser as the two-statement format, whatever punctuation the row used:
    /// queries with and without a trailing `;` must both come out as exactly two statements.
    #[test]
    fn joins_the_pair_into_two_statements() {
        let ddl = "CREATE TABLE t (a int);";
        let got = lower(&row("SELECT a FROM t;", "SELECT t.a FROM t", Some(ddl)), CatalogSource::Declared)
            .expect("lowers");
        assert_eq!(got["queries"].as_array().unwrap().len(), 2);
        assert_eq!(got["schemas"].as_array().unwrap().len(), 1);
    }

    /// Three queries in a row is a corpus error, not a pair to guess at. The join is on `;`, so this
    /// also pins that a stray `;` cannot smuggle a third statement past the parser unnoticed.
    #[test]
    fn refuses_a_row_that_is_not_two_queries() {
        let ddl = "CREATE TABLE t (a int);";
        let e = lower(&row("SELECT a FROM t; SELECT a FROM t", "SELECT a FROM t", Some(ddl)), CatalogSource::Declared)
            .unwrap_err()
            .to_string();
        assert!(e.contains("expected 2 queries"), "{e}");
    }

    /// No DDL under `Declared` is a refusal, not a silent fall-through to inference: the two modes
    /// answer questions about different schemas and the caller picked one.
    #[test]
    fn declared_mode_refuses_a_row_with_no_ddl() {
        let e = lower(&row("SELECT a FROM t", "SELECT a FROM t", None), CatalogSource::Declared)
            .unwrap_err();
        assert_eq!(kind(&e), "schema");
        assert!(e.to_string().contains("no DDL"), "{e}");
    }

    /// The same row under inference has a schema to work with and lowers.
    #[test]
    fn inference_mode_lowers_a_row_with_no_ddl() {
        let got = lower(&row("SELECT a FROM t", "SELECT a FROM t", None), CatalogSource::Inferred);
        assert!(got.is_ok(), "{:?}", got.err().map(|e| e.to_string()));
    }

    #[test]
    fn record_carries_the_refusal_reason() {
        let r = row("SELECT a FROM t", "SELECT b FROM t", None);
        let out = record(&r, &lower(&r, CatalogSource::Declared), false);
        assert_eq!(out["status"], "refuse");
        assert_eq!(out["kind"], "schema");
        assert_eq!(out["name"], "pair0007"); // scrub-ok: an index this test invents
    }

    /// The reflexivity check needs no catalog, so it answers for a row that has no DDL at all —
    /// which is most of the population it exists to reach. The refusal's `kind` and `reason` stay on
    /// the record: what *would* have blocked the row is the datum the coverage roadmap is ranked on,
    /// and this status is the evidence that the ranking was inflated by rows nothing had to lower.
    #[test]
    fn a_reflexive_row_gets_the_third_status_and_keeps_its_reason() {
        let r = row("SELECT a FROM t", "SELECT a FROM t", None);
        assert!(reflexive(&r));
        let out = record(&r, &lower(&r, CatalogSource::Declared), true);
        assert_eq!(out["status"], "reflexive");
        assert_eq!(out["kind"], "schema");
        assert!(!reflexive(&row("SELECT a FROM t", "SELECT b FROM t", None)));
    }
}
