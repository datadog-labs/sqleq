// Unless explicitly stated otherwise all files in this repository are licensed under the
// Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/).
// Copyright 2026-Present Datadog, Inc.

//! A last-line check on the IR the frontend is about to hand the prover.
//!
//! Everything else in this crate refuses what it cannot lower. This module catches the other kind of
//! mistake: IR that *is* emitted, parses fine, and is wrong. It re-derives the prover's variable
//! scoping over the finished JSON and checks the one invariant every column reference has to satisfy.
//!
//! ## The invariant
//!
//! The prover addresses columns by an absolute variable level into a substitution vector, and each
//! relation extends that vector in its own way (`relation.rs`, `Eval<Relation>`): a `Project`
//! evaluates its columns in `subst ++ source_scope`, a `Filter` its condition in
//! `subst ++ source_scope`, a `Join` its condition in `subst ++ left ++ right`, a `Group` its keys
//! and aggregate arguments in `subst ++ source_scope`, and a relation-valued subexpression inherits
//! whatever vector encloses it. So `{"column": n, "type": T}` is well-formed only if `n` indexes
//! that vector and `T` is the type sitting there.
//!
//! ## Why the type half matters as much as the index half
//!
//! `Eval<Expr>` for `Expr::Col` **ignores** the declared `type` and returns `subst[column]`, but
//! `Expr::ty()` — and through it `Relation::scope()` — reads it. A reference that names the right
//! variable with the wrong type therefore makes the prover build a `Project` whose declared row type
//! disagrees with the row it actually produces, and the mismatch surfaces as
//! `assertion left == right failed: $a and $b have different types` from `Logic::Eq`. That assert is
//! a crash, not a refusal, and it only fires for *some* mismatches: an index that lands on a
//! same-typed variable reads the wrong column and is proved against in silence. Checking both halves
//! is what turns the whole class into an error here.
//!
//! ## Cost
//!
//! One walk of the IR, no allocation beyond the scope vectors. It runs on every lowering rather than
//! behind a flag, because the failure it guards against is a wrong *proof*, and a wrong proof is the
//! one outcome this pipeline exists to prevent.

use serde_json::Value;

use crate::error::{schema, Result};

/// Check every column reference in a prover `Input`. `Err` means the IR is ill-formed and must not
/// be emitted.
pub fn check_levels(input: &Value) -> Result<()> {
    let schemas = input["schemas"].as_array().map(Vec::as_slice).unwrap_or(&[]);
    let queries = input["queries"].as_array().map(Vec::as_slice).unwrap_or(&[]);
    for (i, q) in queries.iter().enumerate() {
        let mut v = Verifier { schemas, path: format!("q{i}") };
        v.rel(q, &[])?;
    }
    Ok(())
}

struct Verifier<'a> {
    schemas: &'a [Value],
    path: String,
}

/// The single-key wrapper object each relation node is: `{"project": {..}}`, `{"scan": 3}`, ...
fn tagged(rel: &Value) -> Result<(&str, &Value)> {
    if rel.as_str() == Some("singleton") {
        return Ok(("singleton", rel));
    }
    let obj = rel.as_object().filter(|o| o.len() == 1).ok_or_else(|| ir("not a relation node"))?;
    let (k, v) = obj.iter().next().unwrap();
    Ok((k.as_str(), v))
}

fn ir(msg: impl std::fmt::Display) -> crate::error::FrontendError {
    schema(format!("internal: ill-formed IR: {msg}"))
}

fn ty_of(e: &Value) -> Result<&str> {
    e["type"].as_str().ok_or_else(|| ir("expression without a type"))
}

/// The prover's `AggCall` accepts either spelling for both of its list fields.
fn aggs(v: &Value) -> &[Value] {
    let a = v.get("function").or_else(|| v.get("columns"));
    a.and_then(Value::as_array).map(Vec::as_slice).unwrap_or(&[])
}

fn cols(v: &Value) -> &[Value] {
    let c = v.get("target").or_else(|| v.get("columns"));
    c.and_then(Value::as_array).map(Vec::as_slice).unwrap_or(&[])
}

fn arr<'v>(v: &'v Value, k: &str) -> &'v [Value] {
    v.get(k).and_then(Value::as_array).map(Vec::as_slice).unwrap_or(&[])
}

impl Verifier<'_> {
    /// The column types a relation yields — the prover's `Relation::scope`.
    fn scope(&self, rel: &Value) -> Result<Vec<String>> {
        let (tag, v) = tagged(rel)?;
        let strs = |val: &Value| -> Result<Vec<String>> {
            val.as_array()
                .ok_or_else(|| ir("expected a type list"))?
                .iter()
                .map(|t| t.as_str().map(str::to_string).ok_or_else(|| ir("non-string type")))
                .collect()
        };
        Ok(match tag {
            "singleton" => vec![],
            "scan" => {
                let i = v.as_u64().ok_or_else(|| ir("scan without a table index"))? as usize;
                let t = self.schemas.get(i).ok_or_else(|| ir(format!("scan {i} out of range")))?;
                strs(&t["types"])?
            }
            "filter" | "sort" => self.scope(&v["source"])?,
            "distinct" => self.scope(v)?,
            "project" => cols(v).iter().map(|c| ty_of(c).map(str::to_string)).collect::<Result<_>>()?,
            "aggregate" => aggs(v).iter().map(|a| ty_of(a).map(str::to_string)).collect::<Result<_>>()?,
            "group" => {
                let mut s: Vec<String> =
                    arr(v, "keys").iter().map(|k| ty_of(k).map(str::to_string)).collect::<Result<_>>()?;
                for a in aggs(v) {
                    s.push(ty_of(a)?.to_string());
                }
                s
            }
            "join" | "correlate" => {
                let mut s = self.scope(&v["left"])?;
                if !matches!(v["kind"].as_str(), Some("SEMI") | Some("ANTI")) {
                    s.extend(self.scope(&v["right"])?);
                }
                s
            }
            "union" | "intersect" | "except" => {
                let rels = v.as_array().ok_or_else(|| ir("set op without branches"))?;
                self.scope(rels.first().ok_or_else(|| ir("set op with no branches"))?)?
            }
            "values" => strs(&v["schema"])?,
            other => return Err(ir(format!("unknown relation `{other}`"))),
        })
    }

    fn expr(&mut self, e: &Value, env: &[String], what: &str) -> Result<()> {
        if let Some(c) = e.get("column") {
            let n = c.as_u64().ok_or_else(|| ir("non-numeric column index"))? as usize;
            let declared = ty_of(e)?;
            return match env.get(n) {
                None => Err(ir(format!(
                    "{}/{what}: column {n} is past the end of a {}-wide row",
                    self.path,
                    env.len()
                ))),
                Some(actual) if actual != declared => Err(ir(format!(
                    "{}/{what}: column {n} is declared {declared} but the row has {actual} there",
                    self.path
                ))),
                Some(_) => Ok(()),
            };
        }
        for a in arr(e, "operand") {
            self.expr(a, env, what)?;
        }
        // A relation-valued operand (`IN`, `EXISTS`, a scalar subquery) is evaluated in the very
        // vector its enclosing expression is, which is what makes a subquery's own columns start at
        // the *enclosing* width rather than at zero.
        if let Some(q) = e.get("query") {
            self.rel(q, env)?;
        }
        Ok(())
    }

    fn rel(&mut self, rel: &Value, subst: &[String]) -> Result<()> {
        let (tag, v) = tagged(rel)?;
        let extended = |src: &Value, me: &Self| -> Result<Vec<String>> {
            let mut env = subst.to_vec();
            env.extend(me.scope(src)?);
            Ok(env)
        };
        match tag {
            "singleton" | "scan" => {}
            "distinct" => self.rel(v, subst)?,
            "sort" => self.rel(&v["source"], subst)?,
            "filter" => {
                self.rel(&v["source"], subst)?;
                let env = extended(&v["source"], self)?;
                self.expr(&v["condition"], &env, "filter condition")?;
            }
            "project" => {
                self.rel(&v["source"], subst)?;
                let env = extended(&v["source"], self)?;
                for (i, c) in cols(v).iter().enumerate() {
                    self.expr(c, &env, &format!("projected column {i}"))?;
                }
            }
            "join" | "correlate" => {
                self.rel(&v["left"], subst)?;
                // A correlated join's right side sees the left row; a plain join's does not.
                let mut right_subst = subst.to_vec();
                if tag == "correlate" {
                    right_subst.extend(self.scope(&v["left"])?);
                }
                self.rel(&v["right"], &right_subst)?;
                if let Some(c) = v.get("condition") {
                    let mut env = subst.to_vec();
                    env.extend(self.scope(&v["left"])?);
                    env.extend(self.scope(&v["right"])?);
                    self.expr(c, &env, "join condition")?;
                }
            }
            "group" => {
                self.rel(&v["source"], subst)?;
                let env = extended(&v["source"], self)?;
                for (i, k) in arr(v, "keys").iter().enumerate() {
                    self.expr(k, &env, &format!("group key {i}"))?;
                }
                for a in aggs(v) {
                    let op = a["operator"].as_str().unwrap_or("?");
                    for arg in arr(a, "operand") {
                        self.expr(arg, &env, &format!("argument of {op}"))?;
                    }
                }
            }
            "aggregate" => {
                // `Eval<(AggCall, Relation)>` wraps the arguments in a `Project` over the source, so
                // they are numbered against the source row exactly as a projection would be.
                self.rel(&v["source"], subst)?;
                let env = extended(&v["source"], self)?;
                for a in aggs(v) {
                    let op = a["operator"].as_str().unwrap_or("?");
                    for arg in arr(a, "operand") {
                        self.expr(arg, &env, &format!("argument of {op}"))?;
                    }
                }
            }
            "union" | "intersect" | "except" => {
                for r in v.as_array().ok_or_else(|| ir("set op without branches"))? {
                    self.rel(r, subst)?;
                }
            }
            "values" => {
                // A `Values` row is evaluated one level *above* itself, so only closed expressions
                // are well-formed here — which is what `lower_fromless_select` already guarantees.
                for row in arr(v, "content") {
                    for e in row.as_array().ok_or_else(|| ir("values row is not a list"))? {
                        self.expr(e, &[], "values row")?;
                    }
                }
            }
            other => return Err(ir(format!("unknown relation `{other}`"))),
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::check_levels;

    /// One two-column table, and a query that projects its second column. The subquery in the
    /// filter is where the interesting numbering lives: its own row starts at 2.
    fn input(inner_key: serde_json::Value) -> serde_json::Value {
        json!({
            "schemas": [{ "types": ["INTEGER", "VARCHAR"], "key": [], "nullable": [true, true], "guaranteed": [] }],
            "queries": [{ "project": {
                "target": [{ "column": 0, "type": "INTEGER" }],
                "source": { "filter": {
                    "condition": {
                        "operator": "IN", "type": "BOOLEAN",
                        "operand": [{ "column": 1, "type": "VARCHAR" }],
                        "query": { "group": {
                            "keys": [inner_key], "function": [],
                            "source": { "project": {
                                "target": [{ "column": 3, "type": "VARCHAR" }],
                                "source": { "scan": 0 },
                            }},
                        }},
                    },
                    "source": { "scan": 0 },
                }},
            }}],
            "help": ["", ""],
        })
    }

    #[test]
    fn accepts_a_correctly_numbered_subquery() {
        check_levels(&input(json!({ "column": 2, "type": "VARCHAR" }))).unwrap();
    }

    #[test]
    fn rejects_a_key_numbered_from_zero() {
        // The bug this module exists for: 0 is the *outer* row's first column.
        let e = check_levels(&input(json!({ "column": 0, "type": "VARCHAR" }))).unwrap_err();
        assert!(e.to_string().contains("declared VARCHAR but the row has INTEGER"), "{e}");
    }

    #[test]
    fn rejects_an_index_past_the_end_of_the_row() {
        let e = check_levels(&input(json!({ "column": 7, "type": "VARCHAR" }))).unwrap_err();
        assert!(e.to_string().contains("past the end of a 3-wide row"), "{e}");
    }

    #[test]
    fn a_wrong_type_on_a_right_index_is_still_rejected() {
        // Same variable, wrong label. `Expr::ty` believes the label, `Eval` believes the variable,
        // and the prover asserts they agree.
        let e = check_levels(&input(json!({ "column": 2, "type": "INTEGER" }))).unwrap_err();
        assert!(e.to_string().contains("declared INTEGER but the row has VARCHAR"), "{e}");
    }
}
