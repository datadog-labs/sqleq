// Unless explicitly stated otherwise all files in this repository are licensed under the
// Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/).
// Copyright 2026-Present Datadog, Inc.

//! Package a lowered plan as a verification job for SQLSolver -- the `sqleq-solver` axis, and the
//! JVM fork behind `sqlsolver-jvm`.
//!
//! SQLSolver (SIGMOD 2024, Apache 2.0) is an equivalence prover independent of QED. A job
//! (`sqleq-frontend --sqlsolver --ir <input.json>`,
//! [`ir_job_from_input`](crate::sqlsolver::ir_job_from_input)) is the lowered `Input` the QED prover
//! gets, plus the schema it implies. It is what `sqleq-check` hands `sqleq-solver` (a Rust rewrite of
//! SQLSolver's proof engine) or the JVM fork's `IrDriver`, so the QED prover and the SQLSolver axis
//! read the same bytes. Nothing here lowers anything: the plan arrives already lowered.
//!
//! The schema is **MySQL-dialect DDL**, because the fork's `CalciteSupport` hardcodes
//! `DB_TYPE = MySQL` and so parses it with a MySQL ANTLR grammar. It is derived from the plan's own
//! `schemas` rather than from a second parse of the pair's `CREATE TABLE`s, and printed by
//! [`emit_mysql`](crate::sqlsolver::emit_mysql).
//!
//! ## What is deliberately not declared
//!
//! A `UNIQUE` key is emitted **only when every one of its columns is `NOT NULL`**. SQLSolver models
//! `UNIQUE` as "no duplicate rows at all", where Postgres allows any number of NULLs in a unique
//! column, and the difference is not academic: with a nullable unique `a` it reports
//! `SELECT DISTINCT a FROM t` == `SELECT a FROM t`, which is false in Postgres for a table holding two
//! NULLs. Declaring the key would therefore hand it a premise Postgres does not grant. Dropping
//! it only costs proofs. `NOT NULL` itself is carried, and every key is emitted in the weaker
//! `UNIQUE` spelling rather than a `PRIMARY KEY` that would silently imply `NOT NULL` on top.
//!
//! A plan whose tables share a bare name is refused rather than packaged (see
//! [`ir_job_from_input`](crate::sqlsolver::ir_job_from_input)).

// The prose above and below documents this adapter against the pipeline it adapts, and most
// of that pipeline is private to the crate. The links are for whoever is reading these docs
// with `--document-private-items`; in the public build rustdoc renders them as plain code.
#![allow(rustdoc::private_intra_doc_links)]

use std::collections::BTreeSet;

use serde_json::Value;

use crate::catalog::Catalog;

/// One pair as the **plan-entry bridge** sees it: our lowered IR, and the schema as DDL.
///
/// The shared-frontend direction (`docs/SQLSOLVER.md`, *The IR bridge*): this job carries the IR the
/// qed axis already proved against, for `sqleq-solver` to prove from directly, or for a Java
/// translator to turn into a Calcite `RelNode` and feed to the fork's
/// `Verification.verify(RelNode, RelNode, Schema)`, which skips its SQL front end entirely.
pub struct IrJob {
    pub name: String,
    /// The prover's `Input` JSON, or `None` when the plan cannot be packaged.
    pub ir: Option<Value>,
    pub schema: String,
    pub notes: Vec<&'static str>,
    /// Why `ir` is `None`. Present exactly when it is.
    pub refusal: Option<String>,
}

impl IrJob {
    /// One line of the work file the driver reads.
    pub fn to_json(&self) -> String {
        let mut v = serde_json::json!({ "name": self.name, "schema": self.schema });
        match &self.ir {
            Some(ir) => v["ir"] = ir.clone(),
            None => v["ir"] = Value::Null,
        }
        if let Some(r) = &self.refusal {
            v["refusal"] = serde_json::json!(r);
        }
        if !self.notes.is_empty() {
            v["notes"] = serde_json::json!(self.notes);
        }
        v.to_string()
    }
}

/// Package an already-lowered `Input` as a bridge job: the same bytes, plus the DDL they imply.
///
/// The single-case path (`--sqlsolver --ir <input.json>`), which `sqleq-check` drives. That harness
/// has already lowered the case and handed the JSON to the prover, so re-lowering the SQL here would
/// put a *second* lowering between the two axes -- reintroducing precisely the drift
/// [`ddl_from_ir`] exists to rule out, one level up. Instead the same JSON goes to both provers and
/// the DDL is derived from it.
pub fn ir_job_from_input(name: String, ir: Value) -> IrJob {
    let (schema, duplicate) = ddl_from_ir(&ir);
    let mut notes: Vec<&'static str> = Vec::new();
    if schema.is_empty() {
        notes.push("no-schema");
    }
    // The one soundness gate this path needs. Calcite's root schema is name-keyed and
    // [`emit_mysql`] emits only the first table under a given bare name, so with two tables
    // sharing one name a `{"scan": i}` for the second would address the first's columns —
    // silently proving something about the wrong query. A scan index has no qualified spelling to
    // fall back on, so the plan is refused outright.
    if duplicate {
        return IrJob {
            name,
            ir: None,
            schema: String::new(),
            notes: vec!["duplicate-table-name"],
            refusal: Some("two tables share a bare name; a scan index cannot address them"
                .to_string()),
        };
    }
    IrJob { name, ir: Some(ir), schema, notes, refusal: None }
}

/// The DDL for an `Input`'s `schemas`, rendered by [`emit_mysql`].
///
/// Built from the IR rather than from a second parse of the pair's `CREATE TABLE`s, which is what
/// makes the invariant the bridge depends on hold *by construction*: a `{"scan": i}` addresses
/// `schemas[i]`, so the table Calcite resolves must be the one that entry describes. Two parses of
/// the same DDL could drift — under an inference mode they demonstrably do, since the catalog the IR
/// was lowered against is not the declared one — and the failure would be a proof about the wrong
/// tables rather than an error.
///
/// Column names are synthesized. The IR does not carry them and neither consumer needs them: our
/// scan tuple is addressed by position and so is Calcite's, via `RexInputRef`. Synthetic names are
/// also strictly safer, being unable to collide with a keyword or need quoting.
fn ddl_from_ir(input: &Value) -> (String, bool) {
    let mut tables = Vec::new();
    for (i, s) in input["schemas"].as_array().into_iter().flatten().enumerate() {
        let types: Vec<String> = s["types"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|t| t.as_str().unwrap_or("VARBINARY").to_string())
            .collect();
        let cols: Vec<(String, String)> =
            types.iter().enumerate().map(|(j, t)| (format!("c{j}"), t.clone())).collect();
        let nullable: Vec<bool> = match s["nullable"].as_array() {
            // Absent or short means "not known to be NOT NULL", which is the sound default: a
            // NOT NULL shrinks the instances the prover quantifies over, so inventing one could turn
            // a non-equivalence into a proof. See [`crate::catalog::Table::nullable`].
            Some(n) => (0..cols.len())
                .map(|j| n.get(j).and_then(Value::as_bool).unwrap_or(true))
                .collect(),
            None => vec![true; cols.len()],
        };
        let keys: Vec<Vec<usize>> = s["key"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|k| {
                k.as_array().map(|cs| {
                    cs.iter().filter_map(|c| c.as_u64().map(|u| u as usize)).collect::<Vec<_>>()
                })
            })
            .collect();
        let n_declared = cols.len();
        tables.push(crate::catalog::Table {
            // A schema without a name predates the field rather than naming nothing, so fall back to
            // the positional spelling instead of emitting a table no scan can address.
            name: s["name"].as_str().map(str::to_string).unwrap_or_else(|| format!("t{i}")),
            cols,
            // Like `row_determined`: nothing reads it here, and no DDL spelled the types.
            declared_types: vec![String::new(); n_declared],
            nullable,
            // Like `row_determined`: nothing reads it here, and the conservative value costs nothing.
            opaque_identity: vec![false; n_declared],
            // Post-reduction: this catalog exists to render DDL for the bridge, so nothing reads
            // it. The conservative value costs nothing here.
            row_determined: vec![false; n_declared],
            keys,
            primary_key: Vec::new(),
            n_declared,
            collations: vec![crate::collation::Collation::Default; n_declared],
        });
    }
    emit_mysql(&Catalog { tables, unread: Vec::new() })
}

/// Print a catalog as MySQL-dialect DDL, plus whether two tables shared a bare name.
///
/// Only the first table under a given name is emitted, matching [`Catalog::find`], which resolves a
/// name to its first declaration. The flag is what the caller acts on: an ambiguous bare name means
/// the qualifier rewrite is not injective.
pub fn emit_mysql(cat: &Catalog) -> (String, bool) {
    let mut out = String::new();
    let mut seen: BTreeSet<&str> = BTreeSet::new();
    let mut duplicate = false;
    for t in &cat.tables {
        if !seen.insert(t.name.as_str()) {
            duplicate = true;
            continue;
        }
        let mut lines: Vec<String> = Vec::new();
        for (i, (name, ty)) in t.cols.iter().enumerate() {
            let not_null = if t.nullable[i] { "" } else { " NOT NULL" };
            lines.push(format!("  {} {}{}", ident(name), mysql_type(ty), not_null));
        }
        // See the module docs: a key with a nullable column is a premise Postgres does not give us,
        // so it is dropped rather than weakened.
        for k in t.not_null_keys() {
            let cols: Vec<String> = k.iter().map(|&i| ident(&t.cols[i].0)).collect();
            lines.push(format!("  UNIQUE ({})", cols.join(", ")));
        }
        out.push_str(&format!("CREATE TABLE {} (\n{}\n);\n", ident(&t.name), lines.join(",\n")));
    }
    (out, duplicate)
}

/// A backtick-quoted identifier. MySQL escapes a backtick by doubling it.
fn ident(name: &str) -> String {
    format!("`{}`", name.replace('`', "``"))
}

/// The MySQL spelling of a prover type.
///
/// Total on purpose: a type the match does not name (an opaque column's, say) becomes
/// `varbinary(255)`, never its own spelling. That is load-bearing — their grammar rejects `json`,
/// `uuid`, `inet`, `money` and `xml`, and **one unparseable type name loses the whole `CREATE
/// TABLE`**, so a schema is all-or-nothing per table. All five spellings below were checked against
/// the built jar.
///
/// The temporal types are `int`, as they have always been here. Within one type that is exact (a
/// date is a count of days, a timestamp a count of microseconds), and the IR never lets two temporal
/// types meet except through a conversion, which `IrToRel` turns into an uninterpreted function. A
/// real `date` or `datetime` column type would buy nothing and would lose the column: their set
/// translators throw on any type outside a short numeric-and-string list.
fn mysql_type(prover_type: &str) -> &'static str {
    match prover_type {
        "INTEGER" => "int",
        "DATE" | "TIME" | "TIMESTAMP" | "TIMESTAMPTZ" => "int",
        "REAL" => "double",
        "VARCHAR" => "varchar(255)",
        "BOOLEAN" => "boolean",
        _ => "varbinary(255)",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cat(ddl: &str) -> Catalog {
        crate::pgddl::parse_provided_schema(ddl)
    }

    #[test]
    fn a_postgres_catalog_prints_as_mysql_ddl() {
        let c = cat(r#"CREATE TABLE orders (
                         id integer PRIMARY KEY,
                         total numeric,
                         note text,
                         tags text[],
                         paid boolean NOT NULL,
                         created_at timestamp without time zone NOT NULL
                       );"#);
        let (ddl, dup) = emit_mysql(&c);
        assert!(!dup);
        assert_eq!(
            ddl,
            "CREATE TABLE `orders` (\n  \
               `id` int NOT NULL,\n  \
               `total` double,\n  \
               `note` varchar(255),\n  \
               `tags` varbinary(255),\n  \
               `paid` boolean NOT NULL,\n  \
               `created_at` int NOT NULL,\n  \
               UNIQUE (`id`)\n);\n"
        );
    }

    #[test]
    fn a_key_on_a_nullable_column_is_not_declared() {
        // SQLSolver reads UNIQUE as "no duplicates at all", so declaring this would let it prove
        // `SELECT DISTINCT a` == `SELECT a`, which Postgres does not: two NULLs are allowed.
        let (ddl, _) = emit_mysql(&cat("CREATE TABLE t (a integer UNIQUE, b integer NOT NULL UNIQUE);"));
        assert!(!ddl.contains("UNIQUE (`a`)"), "{ddl}");
        assert!(ddl.contains("UNIQUE (`b`)"), "{ddl}");
    }

    #[test]
    fn only_the_first_table_of_a_name_is_emitted_and_the_clash_is_reported() {
        // A table is keyed on its name as declared, so `a.t` and `b.t` are two names; one name
        // declared twice is the clash.
        let (ddl, dup) = emit_mysql(&cat("CREATE TABLE t (x integer); CREATE TABLE t (y integer);"));
        assert!(dup);
        assert!(ddl.contains("`x`") && !ddl.contains("`y`"), "{ddl}");
    }

    #[test]
    fn a_plan_is_packaged_as_it_came_and_a_shared_bare_name_is_refused() {
        let ir = serde_json::json!({
            "schemas": [{ "name": "t", "types": ["INTEGER"], "nullable": [false], "key": [[0]] }],
            "queries": [{ "scan": 0 }, { "scan": 0 }],
        });
        let job = ir_job_from_input("p".into(), ir.clone());
        assert_eq!(job.ir.as_ref(), Some(&ir), "the same bytes the QED prover got");
        assert_eq!(job.schema, "CREATE TABLE `t` (\n  `c0` int NOT NULL,\n  UNIQUE (`c0`)\n);\n");
        assert!(job.refusal.is_none() && job.notes.is_empty());

        let ir = serde_json::json!({
            "schemas": [{ "name": "t", "types": ["INTEGER"] }, { "name": "t", "types": ["VARCHAR"] }],
            "queries": [{ "scan": 0 }, { "scan": 1 }],
        });
        let job = ir_job_from_input("p".into(), ir);
        assert!(job.ir.is_none() && job.refusal.is_some());
        assert_eq!(job.notes, ["duplicate-table-name"]);
    }
}
