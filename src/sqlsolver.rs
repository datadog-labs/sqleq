// Unless explicitly stated otherwise all files in this repository are licensed under the
// Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/).
// Copyright 2026-Present Datadog, Inc.

//! Emit verification jobs for SQLSolver -- the `sqleq-solver` axis, and the JVM fork behind
//! `sqlsolver-jvm`.
//!
//! SQLSolver (SIGMOD 2024, Apache 2.0) is an equivalence prover independent of QED. Two kinds of
//! job are written here:
//!
//! * **Plan jobs** (`--sqlsolver --ir`): the lowered `Input` the QED prover gets, plus the DDL it
//!   implies ([`ir_job`](crate::sqlsolver::ir_job),
//!   [`ir_job_from_input`](crate::sqlsolver::ir_job_from_input)). These are what `sqleq-check`
//!   hands `sqleq-solver` (a Rust rewrite of SQLSolver's proof engine) or the JVM fork's
//!   `IrDriver`, so the QED prover and the SQLSolver axis read the same bytes.
//! * **SQL-text jobs** (`--sqlsolver` without `--ir`): the row's own SQL plus a schema, for the
//!   original SQLSolver's SQL entry point, which re-derives a plan with Calcite. Nothing in this
//!   repository runs them. That path was wired up first, in the hope of reaching the rows our
//!   frontend refuses without writing another lowering rule; it does not, because Calcite has
//!   refusals of its own and they are not the complement of ours. See `docs/SQLSOLVER.md`.
//!
//! For a text job this module is the adapter, and it does exactly three things to a corpus row:
//!
//! 1. **Renders the schema as MySQL-dialect DDL.** `CalciteSupport` hardcodes `DB_TYPE = MySQL`, so
//!    the schema SQLSolver is handed is parsed by their MySQL ANTLR grammar. We do not write a second
//!    Postgres DDL reader for it: [`crate::pgddl`] already builds a [`Catalog`](crate::catalog::Catalog), and
//!    [`emit_mysql`](crate::sqlsolver::emit_mysql) prints that catalog back out in the dialect their grammar accepts.
//! 2. **Encodes `$N` parameters as 0-ary function calls,** `$1` -> `_DOLLAR_1()`. See below.
//! 3. **Strips schema qualifiers from table references,** `app.foo` -> `foo`. Their Calcite root
//!    schema is flat (`calciteSchema.add(table.name(), calciteTable)`, no sub-schemas), so a qualified
//!    reference cannot resolve — and `pgddl` already keys the catalog on the bare name, so the bare
//!    name is the one spelling both sides can agree on.
//!
//! Everything else about the row is copied through byte for byte. Both rewrites are located by
//! **token span** and applied by splicing the original text, the same discipline as
//! `sqleq-fuzz`'s `rewrite` module: no statement is ever reconstructed from an AST, so formatting,
//! quoting, comments and operator spelling elsewhere in the query cannot be perturbed. Tokenizing
//! rather than parsing also means the rewrites still work on rows that do not parse at all,
//! and that a `$1` inside a string literal, a comment, or a `$$`-quoted body is left alone — the
//! tokenizer has already decided those are one token, and we only edit `Placeholder` tokens.
//!
//! ## Why `$1` becomes `_DOLLAR_1()`
//!
//! Real rewrite pairs are usually parameterized, so without an encoding for `$N` the text path is
//! worth almost nothing. SQLSolver has no dynamic-parameter support:
//! `SqlSupport.parsePreprocess` rewrites
//! `$` -> `_DOLLAR_`, which turns `$1` into the bare identifier `_DOLLAR_1`, which fails Calcite
//! validation and yields `UNKNOWN`.
//!
//! An unresolved *function call* is treated very differently. `CalciteSupport.addUserDefinedFunctions`
//! auto-registers any unknown operator as a UDF, `UExprConcreteTranslator` turns the call into a
//! `UFunc(NON_INT, name, [])` — an uninterpreted function term — and `LiaStarTranslator` maps that
//! term through `uTermToLiaVar.computeIfAbsent(exp, ...)`, keyed on the term itself, so structurally
//! equal terms on the two sides collapse to **one** LIA variable. A 0-ary uninterpreted function is a
//! free constant, and a free constant shared by both sides is exactly parameter semantics: the
//! obligation SQLSolver discharges is `forall params. Q0 == Q1`, not `Q0 == Q1` at some chosen value.
//! Substituting a literal would prove the pair at one value and claim it for all; this does not.
//!
//! Measured directly against the built jar before any of this was written (six probes, all as
//! predicted): `b = $1` vs `$1 = b` is EQ, `b = $1 AND b = $1` vs `b = $1` is EQ, `a + $1` vs
//! `$1 + a` is EQ — one shared symbol, usable as a value and not just in a predicate — while
//! `b = $1` vs `b = $2` is not EQ and `b = $1` vs `b = 0` is not EQ. The encoding also passes
//! `SqlSupport`'s `$`-mangling guard untouched, because the text it produces already contains
//! `_DOLLAR_`.
//!
//! Like the QED path, this is **index binding**: `$1` on the left is `$1` on the right. That is the
//! assumption [`crate::params`] exists to police, so the same arity evidence is recorded here as a
//! note (never a refusal — on the text path the verdict is the measurement).
//!
//! ## What is deliberately not declared
//!
//! A `UNIQUE` key is emitted **only when every one of its columns is `NOT NULL`**. SQLSolver models
//! `UNIQUE` as "no duplicate rows at all", where Postgres allows any number of NULLs in a unique
//! column, and the difference is not academic: with a nullable unique `a` it reports
//! `SELECT DISTINCT a FROM t` == `SELECT a FROM t`, which is false in Postgres for a table holding two
//! NULLs. Declaring the key would therefore hand it a premise Postgres does not grant. Dropping
//! it only costs proofs. `NOT NULL` itself is carried, and `PRIMARY KEY` is not distinguishable from
//! `UNIQUE` once `pgddl` has read the DDL, so every key is emitted in the weaker `UNIQUE` spelling
//! rather than a `PRIMARY KEY` that would silently imply `NOT NULL` on top.
//!
//! ## The text path refuses nothing
//!
//! A row whose DDL yields no tables still gets a job with an empty schema, and a row whose queries
//! do not tokenize still gets a job with its text unmodified. What SQLSolver does with them is the
//! measurement. The notes on a [`Job`](crate::sqlsolver::Job) say which rewrites did not fire, so
//! the harness can report that separately from the verdict. A plan job, by contrast, carries the
//! frontend's refusal when the row did not lower, and is refused when two tables share a bare name
//! (see [`ir_job_from_input`](crate::sqlsolver::ir_job_from_input)).

// The prose above and below documents this adapter against the pipeline it adapts, and most
// of that pipeline is private to the crate. The links are for whoever is reading these docs
// with `--document-private-items`; in the public build rustdoc renders them as plain code.
#![allow(rustdoc::private_intra_doc_links)]

use std::collections::{BTreeMap, BTreeSet};

use sqlparser::tokenizer::{Location, Token, TokenWithSpan, Tokenizer};

use serde_json::Value;

use crate::catalog::Catalog;
use crate::corpus::Row;
use crate::{pgddl, CatalogSource, DIALECT};

/// One pair as SQLSolver should see it: the two queries after both rewrites, and the schema in the
/// dialect its parser reads.
pub struct Job {
    pub name: String,
    pub sql0: String,
    pub sql1: String,
    pub schema: String,
    /// What the rewrites did, or could not do. Never a refusal — see the module docs.
    pub notes: Vec<&'static str>,
}

impl Job {
    /// One line of the work file the driver reads.
    pub fn to_json(&self) -> String {
        let mut v = serde_json::json!({
            "name": self.name,
            "sql0": self.sql0,
            "sql1": self.sql1,
            "schema": self.schema,
        });
        if !self.notes.is_empty() {
            v["notes"] = serde_json::json!(self.notes);
        }
        serde_json::to_string(&v).expect("serialize")
    }
}

/// Build the job for a corpus row, from the row's own SQL.
pub fn job(row: &Row) -> Job {
    build(row, false)
}

/// [`job`], but with the two queries replaced by the SQL our own normalizations produce.
///
/// The composition experiment: SQLSolver's ladder is mostly a rewriter (see `docs/SQLSOLVER.md`), so
/// question is whether *our* rewriter feeds it — whether pairs it cannot simplify to a common form
/// become pairs it can, once ours has run. Everything else about the job is identical, so the two
/// runs differ in exactly one thing and the comparison is a controlled one.
///
/// A row is normalized only when [`crate::corpus::reflexive_forms`] returns a pair, which needs the
/// input to parse and to hold exactly two non-DDL statements; otherwise the row keeps its raw SQL and
/// is marked `not-normalized`, so the two runs still cover the same names and the population that
/// actually changed is readable off the notes.
///
/// **The one guard that cannot be recomputed afterwards** is [`qualifier_conflict`], which is
/// cross-side: `normalize::strip_schema` has a per-query injectivity check but no view of the other
/// side, so it can strip `x.t` on one side and `y.t` on the other and merge two tables into one. That
/// check therefore runs on the raw text *before* normalizing, and a row it fires on is not normalized
/// at all — the collapse it prevents is not reversible once rendered.
pub fn job_normalized(row: &Row) -> Job {
    build(row, true)
}

fn build(row: &Row, normalized: bool) -> Job {
    let cat = match &row.ddl {
        Some(d) => pgddl::parse_provided_schema(d),
        None => Catalog { tables: Vec::new() },
    };
    let mut notes: Vec<&'static str> = Vec::new();
    let (schema, duplicate_name) = emit_mysql(&cat);
    if duplicate_name {
        notes.push("duplicate-table-name");
    }
    if cat.tables.is_empty() {
        notes.push("no-schema");
    }

    let a = analyze(&row.a, &cat);
    let b = analyze(&row.b, &cat);
    if a.is_none() || b.is_none() {
        notes.push("untokenized");
    }

    // A row this note is on must never be read as proved, whatever SQLSolver answers for it — see
    // [`is_query`]. A side we could not tokenise counts as not-a-query, because we cannot show it is
    // one. Whoever runs a text job (nothing in this repository does) must quarantine on the note;
    // the emitter keeps the row, so the verdict is still measured and an `EQ` we refused stays
    // visible in the record.
    if !a.as_ref().is_some_and(|a| a.query) || !b.as_ref().is_some_and(|b| b.query) {
        notes.push("non-select-statement");
    }

    // The qualifier rewrite is a renaming, and a renaming is only safe when it is injective: if two
    // different qualifiers reduce to the same bare table, `a.t` and `b.t` would become one relation
    // and a non-equivalent pair could read as equivalent. Detect that and leave both sides qualified
    // instead, which cannot resolve against their flat schema and so cannot yield a proof either.
    // The query condition does fire in practice. The duplicate-DDL one has not been observed, but it
    // is the same hazard reached by another route and the guard for it is one `||`.
    let ambiguous = duplicate_name
        || match (&a, &b) {
            (Some(a), Some(b)) => qualifier_conflict(&[a, b]),
            _ => false,
        };
    if ambiguous {
        notes.push("ambiguous-qualifier");
    }

    let strip = !ambiguous;
    let sql0 = a.as_ref().map(|a| apply(&row.a, a, strip)).unwrap_or_else(|| row.a.clone());
    let sql1 = b.as_ref().map(|b| apply(&row.b, b, strip)).unwrap_or_else(|| row.b.clone());

    if let (Some(a), Some(b)) = (&a, &b) {
        if a.odd_placeholder || b.odd_placeholder {
            notes.push("odd-placeholder");
        }
        if misaligned(a, b) {
            notes.push("parameter-misaligned");
        }
    }

    let (sql0, sql1) = match normalized {
        false => (sql0, sql1),
        true => match normalized_sides(row, &cat, ambiguous) {
            Some(pair) => {
                notes.push("normalized");
                pair
            }
            None => {
                notes.push("not-normalized");
                (sql0, sql1)
            }
        },
    };
    Job { name: row.name(), sql0, sql1, schema, notes }
}

/// The two sides after [`crate::Rewrites::ALL`], rendered and put through the same two rewrites the
/// raw path applies. `None` when the row cannot be normalized, or must not be — see [`job_normalized`].
///
/// Re-tokenizing the *rendered* text is what makes this cheap: [`apply`] never needed the original
/// bytes, only a tokenizable string, and sqlparser prints a `$N` placeholder back as `$N`. So the
/// `_DOLLAR_N()` encoding and the qualifier strip are the identical code on the identical discipline,
/// and nothing about the parameter argument in the module docs changes.
///
/// The quarantine notes stay on the raw analysis deliberately: `non-select-statement` is a safety
/// property, and taking it from the raw text means normalizing can only ever leave it on, never lift
/// it. A normalization does not turn DML into a query, so the two agree in fact; this makes it hold
/// by construction.
fn normalized_sides(row: &Row, cat: &Catalog, ambiguous: bool) -> Option<(String, String)> {
    if ambiguous {
        return None;
    }
    let (n0, n1) = crate::corpus::reflexive_forms(row, crate::Rewrites::ALL)?;
    let a = analyze(&n0, cat)?;
    let b = analyze(&n1, cat)?;
    Some((apply(&n0, &a, true), apply(&n1, &b, true)))
}

/// One pair as the **plan-entry bridge** sees it: our lowered IR, and the schema as DDL.
///
/// The shared-frontend direction (`docs/SQLSOLVER.md`, *The IR bridge*). [`Job`] hands SQLSolver SQL
/// text, which its Calcite front end re-parses with a MySQL grammar — and that parse is where the
/// sqlsolver axis actually loses: most of its no-proofs are the parser, not the prover. This job
/// instead carries the IR the qed axis already proved against, for a Java translator to turn into a
/// Calcite `RelNode` and feed to `Verification.verify(RelNode, RelNode, Schema)`, which skips their
/// front end entirely.
///
/// A row that does not lower is still emitted, with `ir: null` and the refusal recorded. The two runs
/// then cover the same names and the population that changed is readable off the notes — the same
/// argument as [`job_normalized`]'s `not-normalized`.
pub struct IrJob {
    pub name: String,
    /// The prover's `Input` JSON, or `None` when the row does not lower.
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

/// Lower one row for the bridge: [`crate::corpus::lower`] plus the DDL its IR implies.
///
/// `source` must be the mode the qed axis runs, or the comparison measures two different frontends
/// rather than two provers. Batch runs use the inferring mode; under [`CatalogSource::Declared`]
/// every parameterized row refuses, which on a real corpus leaves next to nothing to emit, so passing
/// the wrong one silently reduces this integration to nothing.
pub fn ir_job(row: &Row, source: CatalogSource) -> IrJob {
    let name = row.name();
    match crate::corpus::lower(row, source) {
        Err(e) => IrJob {
            name,
            ir: None,
            schema: String::new(),
            notes: Vec::new(),
            refusal: Some(e.to_string()),
        },
        Ok(ir) => ir_job_from_input(name, ir),
    }
}

/// Package an already-lowered `Input` as a bridge job: the same bytes, plus the DDL they imply.
///
/// Split out of [`ir_job`] for the single-case path (`--sqlsolver --ir <input.json>`), which
/// `sqleq-check` drives. That harness has already lowered the case and handed the JSON to
/// the prover, so re-lowering the SQL here would put a *second* lowering between the two axes --
/// reintroducing precisely the drift [`ddl_from_ir`] exists to rule out, one level up. Instead the
/// same JSON goes to both provers and the DDL is derived from it.
pub fn ir_job_from_input(name: String, ir: Value) -> IrJob {
    let (schema, duplicate) = ddl_from_ir(&ir);
    let mut notes: Vec<&'static str> = Vec::new();
    if schema.is_empty() {
        notes.push("no-schema");
    }
    // The one soundness gate this path needs. Calcite's root schema is name-keyed and
    // [`emit_mysql`] emits only the first table under a given bare name, so with two tables
    // sharing one name a `{"scan": i}` for the second would address the first's columns —
    // silently proving something about the wrong query. The text path can leave both sides
    // qualified instead and let the reference fail to resolve; a scan index has no such
    // spelling to fall back on, so the row is refused outright.
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
/// Built from the IR rather than from a second parse of the row's `CREATE TABLE`s, which is what
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
            nullable,
            // Post-reduction: this catalog exists to render DDL for the bridge, so nothing reads
            // it. The conservative value costs nothing here.
            row_determined: vec![false; n_declared],
            keys,
            n_declared,
        });
    }
    emit_mysql(&Catalog { tables })
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
        for k in &t.keys {
            // See the module docs: a key with a nullable column is a premise Postgres does not give
            // us, so it is dropped rather than weakened.
            if k.is_empty() || k.iter().any(|&i| t.nullable.get(i).copied().unwrap_or(true)) {
                continue;
            }
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
/// The domain is closed: [`pgddl`] types every column through `map_pg_type`, which returns one of
/// the five builtin names or a temporal one, and falls back to `VARBINARY` for everything it does not
/// recognise. That closure is load-bearing — their grammar rejects `json`, `uuid`, `inet`, `money`
/// and `xml`, and **one unparseable type name loses the whole `CREATE TABLE`**, so a schema is
/// all-or-nothing per table. All five spellings below were checked against the built jar.
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

/// A dotted identifier chain, as token indices, and how much of its head is a schema qualifier.
struct Chain {
    /// The chain's identifier parts.
    parts: Vec<usize>,
    /// The separating periods: `dots[i]` sits between `parts[i]` and `parts[i + 1]`.
    dots: Vec<usize>,
    /// How many leading parts are a schema/catalog qualifier, so how many to drop. 0 = leave alone.
    drop: usize,
}

/// A query's token stream and everything in it either rewrite needs to find.
struct Analysis {
    toks: Vec<TokenWithSpan>,
    chains: Vec<Chain>,
    /// `$N` placeholders: token index and index number.
    params: Vec<(usize, u32)>,
    /// A placeholder that is not `$<digits>` — `?`, or `$` followed by a name. Left untouched.
    odd_placeholder: bool,
    /// Whether the statement is a `SELECT`/`WITH` query rather than DML. See [`is_query`].
    query: bool,
}

/// Tokenize a query and locate the chains and placeholders. `None` if it does not tokenize.
fn analyze(sql: &str, cat: &Catalog) -> Option<Analysis> {
    let toks = Tokenizer::new(&DIALECT, sql).tokenize_with_location().ok()?;
    let mut chains = Vec::new();
    let mut params = Vec::new();
    let mut odd_placeholder = false;
    let mut i = 0;
    while i < toks.len() {
        match &toks[i].token {
            Token::Placeholder(p) => {
                match p.strip_prefix('$').and_then(|n| n.parse::<u32>().ok()) {
                    Some(n) => params.push((i, n)),
                    None => odd_placeholder = true,
                }
                i += 1;
            }
            Token::Word(_) => {
                // Walk the whole chain, then resume after it, so an inner part cannot start a second
                // chain and be mistaken for a qualifier.
                let mut parts = vec![i];
                let mut dots = Vec::new();
                let mut last = i;
                while let Some(d) = significant(&toks, last + 1) {
                    if !matches!(toks[d].token, Token::Period) {
                        break;
                    }
                    let Some(w) = significant(&toks, d + 1) else { break };
                    if !matches!(toks[w].token, Token::Word(_)) {
                        break;
                    }
                    dots.push(d);
                    parts.push(w);
                    last = w;
                }
                if parts.len() > 1 {
                    let words: Vec<String> =
                        parts.iter().map(|&p| word(&toks[p]).unwrap_or_default()).collect();
                    let drop = qualifier_len(&words, cat);
                    if drop > 0 {
                        chains.push(Chain { parts, dots, drop });
                    }
                }
                i = last + 1;
            }
            _ => i += 1,
        }
    }
    let query = is_query(&toks);
    Some(Analysis { toks, chains, params, odd_placeholder, query })
}

/// Whether the statement is a query rather than DML.
///
/// This exists because SQLSolver reports `EQ` for *any* two `UPDATE`s against the same table.
/// `VerificationImpl.getVerifyResult` consults `PlanSupport.isLiteralEq` before it does any solving,
/// and `isLiteralEq` re-parses both sides with the legacy WeTune MySQL parser, whose grammar reduces
/// `UPDATE t SET … WHERE …` to the bare table reference `t`. Both sides assemble to `Input{t}`, the
/// structural comparison succeeds, and `EQ` is returned having compared neither the SET list nor the
/// WHERE clause. Measured: `UPDATE t SET a=1 WHERE b=2 AND c=3` ≡ `UPDATE t SET a=99 WHERE b=2`, and
/// even `UPDATE t SET a=1 WHERE b IN (SELECT b FROM u)` ≡ `UPDATE t SET a=1`.
///
/// `INSERT` and `DELETE` reduce to the same bare table but `assemblePlan` then returns null, so the
/// comparison fails and the pair falls through to `UNKNOWN`. That is safety by accident, not by
/// design, so the rule here is the structural one — a statement that is not a query is not something
/// this prover models — rather than a list of the keywords that happen to be dangerous today.
///
/// `SELECT` is unaffected: the legacy parser either parses a query in full or returns nothing. The
/// one clause it drops silently is `FOR UPDATE`, which changes locking and not the returned rows, so
/// letting the two compare equal is correct.
fn is_query(toks: &[TokenWithSpan]) -> bool {
    let mut i = 0;
    // A query may be wrapped in parentheses — `(SELECT …) UNION (SELECT …)`.
    while let Some(k) = significant(toks, i) {
        if !matches!(toks[k].token, Token::LParen) {
            return matches!(word(&toks[k]).as_deref(), Some("select" | "with" | "table" | "values"));
        }
        i = k + 1;
    }
    false
}

/// How many leading parts of a dotted chain are a schema/catalog qualifier.
///
/// The anchor is the first part that names a declared table: everything before it qualifies that
/// table, everything after it is a column or a `*`. So `app.foo.id` drops one part when `foo` is
/// declared, and `t.col` drops none when `t` is an alias. A chain with no declared table in it is
/// left alone — except that three parts cannot be `table.column`, so its head is a qualifier either
/// way; that case is a `catalog.schema.table` whose table we have no DDL for, and dropping the head
/// at least stops us from asking about a name that certainly cannot resolve.
///
/// Both errors this can make are one-directional. Stripping too much turns the query into one
/// Calcite cannot validate, which is `UNKNOWN`; stripping too little leaves a reference that cannot
/// resolve, which is also `UNKNOWN`. Neither invents a proof — that is what the injectivity check in
/// [`qualifier_conflict`] is for.
fn qualifier_len(words: &[String], cat: &Catalog) -> usize {
    for (k, w) in words.iter().enumerate() {
        if cat.find(w).is_some() {
            return k;
        }
    }
    usize::from(words.len() >= 3)
}

/// Do two chains reduce two *different* qualified names to the same bare table?
fn qualifier_conflict(sides: &[&Analysis]) -> bool {
    let mut seen: BTreeMap<String, String> = BTreeMap::new();
    for a in sides {
        for c in &a.chains {
            let bare = word(&a.toks[c.parts[c.drop]]).unwrap_or_default();
            let qual: Vec<String> =
                c.parts[..c.drop].iter().map(|&p| word(&a.toks[p]).unwrap_or_default()).collect();
            let qual = qual.join(".");
            match seen.get(&bare) {
                Some(prev) if *prev != qual => return true,
                Some(_) => {}
                None => {
                    seen.insert(bare, qual);
                }
            }
        }
    }
    false
}

/// The evidence [`crate::params::check_arity`] refuses on, recorded here as a note: the two sides
/// mention different sets of `$N` *and* share at least one, so index binding may not be the question
/// the pair was written to ask.
fn misaligned(a: &Analysis, b: &Analysis) -> bool {
    let sa: BTreeSet<u32> = a.params.iter().map(|&(_, n)| n).collect();
    let sb: BTreeSet<u32> = b.params.iter().map(|&(_, n)| n).collect();
    sa != sb && sa.intersection(&sb).next().is_some()
}

/// Index of the next token that is not whitespace or a comment (sqlparser reports both as
/// `Whitespace`), at or after `from`.
fn significant(toks: &[TokenWithSpan], from: usize) -> Option<usize> {
    (from..toks.len()).find(|&i| !matches!(toks[i].token, Token::Whitespace(_)))
}

/// A `Word` token's text, unquoted.
fn word(t: &TokenWithSpan) -> Option<String> {
    match &t.token {
        Token::Word(w) => Some(w.value.to_lowercase()),
        _ => None,
    }
}

/// Apply both rewrites to a query by splicing its original bytes.
fn apply(sql: &str, a: &Analysis, strip_qualifiers: bool) -> String {
    let mut edits: Vec<(usize, usize, String)> = Vec::new();
    for &(i, n) in &a.params {
        if let Some(r) = range(sql, &a.toks[i]) {
            edits.push((r.0, r.1, format!("_DOLLAR_{n}()")));
        }
    }
    if strip_qualifiers {
        for c in &a.chains {
            // From the head of the chain through the period before the anchor: `app.` goes,
            // `foo.id` stays.
            let head = range(sql, &a.toks[c.parts[0]]);
            let dot = range(sql, &a.toks[c.dots[c.drop - 1]]);
            if let (Some(h), Some(d)) = (head, dot) {
                edits.push((h.0, d.1, String::new()));
            }
        }
    }
    splice(sql, edits)
}

/// A token's byte range in `sql`, or `None` if either end has no usable location.
fn range(sql: &str, t: &TokenWithSpan) -> Option<(usize, usize)> {
    let s = byte_of(sql, t.span.start)?;
    let e = byte_of(sql, t.span.end)?;
    (s <= e).then_some((s, e))
}

/// Byte offset of a 1-based (line, char-column) [`Location`].
///
/// sqlparser counts columns in `char`s, not bytes, so a query containing any multi-byte character
/// before the token would put a naive `column - 1` inside a character. Walking `char_indices` is the
/// conversion that cannot be off. `None` for the `line: 0` empty span sqlparser reports when it has
/// no location, and for a location past the end of `sql`.
fn byte_of(sql: &str, l: Location) -> Option<usize> {
    if l.line == 0 || l.column == 0 {
        return None;
    }
    let (mut line, mut col) = (1u64, 1u64);
    for (b, c) in sql.char_indices() {
        if line == l.line && col == l.column {
            return Some(b);
        }
        if c == '\n' {
            line += 1;
            col = 1;
        } else {
            col += 1;
        }
    }
    (line == l.line && col == l.column).then_some(sql.len())
}

/// Replace the given byte ranges and copy every other byte through. Overlapping edits cannot happen
/// — a placeholder is not a chain — but a later edit that starts inside an earlier one is dropped
/// rather than allowed to corrupt the output.
fn splice(sql: &str, mut edits: Vec<(usize, usize, String)>) -> String {
    edits.sort_by_key(|e| e.0);
    let mut out = String::with_capacity(sql.len());
    let mut at = 0usize;
    for (s, e, text) in edits {
        if s < at || e > sql.len() {
            continue;
        }
        out.push_str(&sql[at..s]);
        out.push_str(&text);
        at = e;
    }
    out.push_str(&sql[at..]);
    out
}

#[cfg(test)]
mod tests {

    #[test]
    fn dml_is_quarantined_and_queries_are_not() {
        // SQLSolver reports EQ for any two UPDATEs on the same table, so a row carrying one must
        // never be counted as proved. See `is_query` for the mechanism.
        let q = |sql: &str| {
            let cat = Catalog { tables: Vec::new() };
            analyze(sql, &cat).map(|a| a.query)
        };
        for sql in [
            "SELECT 1",
            "  \n -- lead\n SELECT 1",
            "/* c */ WITH x AS (SELECT 1) SELECT * FROM x",
            "(SELECT 1) UNION (SELECT 2)",
            "VALUES (1), (2)",
        ] {
            assert_eq!(q(sql), Some(true), "{sql}");
        }
        for sql in [
            "UPDATE t SET a = 1 WHERE b = 2",
            "DELETE FROM t WHERE b = 2",
            "INSERT INTO t VALUES (1)",
            "UPDATE t SET a = 1 RETURNING a",
        ] {
            assert_eq!(q(sql), Some(false), "{sql}");
        }
    }

    use super::*;

    fn cat(ddl: &str) -> Catalog {
        pgddl::parse_provided_schema(ddl)
    }

    fn rewrite(sql: &str, c: &Catalog) -> String {
        let a = analyze(sql, c).expect("tokenizes");
        apply(sql, &a, true)
    }

    fn row(a: &str, b: &str, ddl: Option<&str>) -> Row {
        Row { index: 7, a: a.into(), b: b.into(), ddl: ddl.map(str::to_string) }
    }

    #[test]
    fn the_normalized_job_carries_the_normalized_sql_and_says_so() {
        // The lever that produced most of the measured gains: an `ORDER BY .. LIMIT $N` sandwich
        // identical on both sides, which SQLSolver has no model for and ours removes.
        let r = row(
            "SELECT a FROM t WHERE a > $1 ORDER BY a DESC LIMIT $2;",
            "SELECT a FROM t WHERE $1 < a ORDER BY a DESC LIMIT $2;",
            Some("CREATE TABLE t (a integer);"),
        );
        let raw = job(&r);
        assert!(raw.sql0.contains("LIMIT"), "{}", raw.sql0);
        assert!(!raw.notes.contains(&"normalized"));

        let n = job_normalized(&r);
        assert!(n.notes.contains(&"normalized"));
        assert!(!n.sql0.contains("LIMIT"), "{}", n.sql0);
        assert!(!n.sql1.contains("LIMIT"), "{}", n.sql1);
        // The parameter encoding survives the round trip through the AST: `apply` re-tokenizes the
        // rendered text, so it finds the placeholders exactly as it does in the raw path.
        assert!(n.sql0.contains("_DOLLAR_1()"), "{}", n.sql0);
        assert!(!n.sql0.contains('$'), "{}", n.sql0);
        // Same schema either way -- the two runs differ in the queries and nothing else.
        assert_eq!(raw.schema, n.schema);
        assert_eq!(raw.name, n.name);
    }

    #[test]
    fn a_row_that_does_not_normalize_keeps_its_own_sql() {
        // Not a pair: three statements. `reflexive_forms` declines, and the job must still be the
        // raw one so the two runs cover the same names.
        let r = row("SELECT 1;", "SELECT 2; SELECT 3;", None);
        let n = job_normalized(&r);
        assert!(n.notes.contains(&"not-normalized"));
        assert_eq!((n.sql0.as_str(), n.sql1.as_str()), (job(&r).sql0.as_str(), job(&r).sql1.as_str()));
    }

    #[test]
    fn an_ambiguous_qualifier_is_never_normalized() {
        // `strip_schema`'s injectivity check is per query; `qualifier_conflict` is cross-side. Two
        // different schemas reducing to one bare table would merge two relations, and rendering the
        // strip makes it irreversible -- so the row keeps its qualified text instead.
        let r = row(
            "SELECT x FROM one.t;",
            "SELECT x FROM two.t;",
            Some("CREATE TABLE one.t (x integer);"),
        );
        let n = job_normalized(&r);
        assert!(n.notes.contains(&"ambiguous-qualifier"));
        assert!(n.notes.contains(&"not-normalized"));
        assert!(n.sql0.contains("one.t"), "{}", n.sql0);
        assert!(n.sql1.contains("two.t"), "{}", n.sql1);
    }

    #[test]
    fn a_postgres_catalog_prints_as_mysql_ddl() {
        let c = cat(r#"CREATE TABLE public.orders (
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
        let (ddl, dup) = emit_mysql(&cat("CREATE TABLE a.t (x integer); CREATE TABLE b.t (y integer);"));
        assert!(dup);
        assert!(ddl.contains("`x`") && !ddl.contains("`y`"), "{ddl}");
    }

    #[test]
    fn placeholders_become_nullary_calls() {
        let c = cat("CREATE TABLE t (a integer, b integer);");
        assert_eq!(
            rewrite("SELECT a + $1 FROM t WHERE b = $2 AND a = $1", &c),
            "SELECT a + _DOLLAR_1() FROM t WHERE b = _DOLLAR_2() AND a = _DOLLAR_1()"
        );
        // Two digits, and a placeholder immediately followed by punctuation.
        assert_eq!(
            rewrite("SELECT * FROM t WHERE a IN ($10,$2)", &c),
            "SELECT * FROM t WHERE a IN (_DOLLAR_10(),_DOLLAR_2())"
        );
    }

    #[test]
    fn a_dollar_that_is_not_a_placeholder_is_left_alone() {
        let c = cat("CREATE TABLE t (a integer, note text);");
        // A string literal, a line comment, a block comment, and a `$$`-quoted body: the tokenizer
        // has already decided each is one token, and we only ever edit `Placeholder`.
        let sql = "SELECT '$1' AS s, -- $2 in a comment\n  /* $3 too */ $$ $4 in a body $$ FROM t WHERE a = $5";
        assert_eq!(
            rewrite(sql, &c),
            "SELECT '$1' AS s, -- $2 in a comment\n  /* $3 too */ $$ $4 in a body $$ FROM t WHERE a = _DOLLAR_5()"
        );
    }

    #[test]
    fn a_query_that_does_not_parse_still_gets_its_parameters_encoded() {
        let c = cat("CREATE TABLE t (a integer);");
        // Not a parseable statement; it tokenizes fine, which is all the rewrite needs.
        assert_eq!(rewrite("SELECT a FROM t WHERE a = $1 FILTER GARBAGE (", &c),
                   "SELECT a FROM t WHERE a = _DOLLAR_1() FILTER GARBAGE (");
    }

    #[test]
    fn schema_qualifiers_come_off_tables_and_not_off_columns() {
        let c = cat("CREATE TABLE app.foo (id integer, note text);");
        assert_eq!(rewrite("SELECT f.id FROM app.foo f", &c), "SELECT f.id FROM foo f");
        assert_eq!(
            rewrite("SELECT app.foo.id FROM app.foo", &c),
            "SELECT foo.id FROM foo"
        );
        assert_eq!(rewrite("SELECT app.foo.* FROM app.foo", &c), "SELECT foo.* FROM foo");
        // `foo` is declared, so it anchors the chain and nothing is dropped.
        assert_eq!(rewrite("SELECT foo.id FROM foo", &c), "SELECT foo.id FROM foo");
        // Three parts with no declared table: the head cannot be a table, so it goes.
        assert_eq!(rewrite("SELECT x FROM db.other.bar", &c), "SELECT x FROM other.bar");
        // Whitespace and comments between the parts are inside the spliced range. The blank the
        // qualifier used to sit against stays, because nothing deletes bytes it did not locate.
        assert_eq!(rewrite("SELECT id FROM app /*x*/ . foo", &c), "SELECT id FROM  foo");
    }

    #[test]
    fn an_ambiguous_qualifier_leaves_both_sides_qualified() {
        let c = cat("CREATE TABLE t (a integer);");
        let a = analyze("SELECT a FROM x.t", &c).unwrap();
        let b = analyze("SELECT a FROM y.t", &c).unwrap();
        assert!(qualifier_conflict(&[&a, &b]));
        assert!(!qualifier_conflict(&[&a, &a]));
        assert_eq!(apply("SELECT a FROM x.t", &a, false), "SELECT a FROM x.t");
    }

    #[test]
    fn both_rewrites_compose_on_one_query() {
        let c = cat("CREATE TABLE laps.runs (id integer, t integer);");
        assert_eq!(
            rewrite("SELECT laps.runs.id FROM laps.runs WHERE laps.runs.t > $1", &c),
            "SELECT runs.id FROM runs WHERE runs.t > _DOLLAR_1()"
        );
    }

    #[test]
    fn misalignment_is_noted_only_when_the_index_sets_overlap() {
        let c = cat("CREATE TABLE t (a integer, b integer);");
        let one = analyze("SELECT a FROM t WHERE a = $1", &c).unwrap();
        let two = analyze("SELECT a FROM t WHERE a = $1 AND b = $2", &c).unwrap();
        let other = analyze("SELECT a FROM t WHERE a = $2", &c).unwrap();
        assert!(misaligned(&one, &two));
        assert!(!misaligned(&one, &other));
        assert!(!misaligned(&one, &one));
    }
}
