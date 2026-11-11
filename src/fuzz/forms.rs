// Unless explicitly stated otherwise all files in this repository are licensed under the
// Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/).
// Copyright 2026-Present Datadog, Inc.

//! What both engines read off a pair before running it: the names each table is referred to by,
//! and the rows generated for it. Kept apart from either engine, so the Postgres engine builds
//! without DuckDB.

use std::collections::{BTreeMap, BTreeSet};
use std::ops::ControlFlow;

use sqlparser::ast::{visit_relations, ObjectName, ObjectNamePart};
use sqlparser::dialect::PostgreSqlDialect;
use sqlparser::parser::Parser;

use crate::fuzz::gen::Val;
use crate::fuzz::schema::{resolve, Schema};

/// Final table name -> the set of qualified name-part lists the queries reference it by.
pub type Forms = BTreeMap<String, BTreeSet<Vec<String>>>;
/// Final table name -> generated rows (each row aligned with the table's columns).
pub type RowData = BTreeMap<String, Vec<Vec<Val>>>;

/// Lower-cased dotted parts of an object name (identifiers only).
fn name_parts(n: &ObjectName) -> Vec<String> {
    n.0.iter()
        .filter_map(|p| match p {
            ObjectNamePart::Identifier(id) => Some(id.value.to_lowercase()),
            _ => None,
        })
        .collect()
}

/// Discover the qualified forms each known table is referenced by across the pair, so we create it
/// under exactly that name. Falls back to bare names when either query does not parse.
pub fn table_forms(a: &str, b: &str, schema: &Schema) -> Forms {
    let dialect = PostgreSqlDialect {};
    let mut forms: Forms = BTreeMap::new();
    let parsed_a = Parser::parse_sql(&dialect, a).ok();
    let parsed_b = Parser::parse_sql(&dialect, b).ok();
    if let (Some(sa), Some(sb)) = (&parsed_a, &parsed_b) {
        for st in sa.iter().chain(sb.iter()) {
            let _ = visit_relations(st, |name: &ObjectName| {
                let parts = name_parts(name);
                if let Some(key) = resolve(schema, &parts) {
                    forms.entry(key.clone()).or_default().insert(parts.clone());
                }
                ControlFlow::<()>::Continue(())
            });
        }
    }
    if forms.is_empty() {
        // Regex fallback, used when either side fails to parse. It must recover the *qualified*
        // form and not just the bare name: creating `people` when the query reads `crm.people`
        // leaves DuckDB reporting that schema "crm" does not exist, which loses the pair even
        // though we hold a perfectly good table for it.
        //
        // Quotes are stripped first so `"crm"."people"` reduces to the dotted form. The
        // leading `[^\w.]` guard stops a table name matching the tail of a longer identifier
        // (`user_products` must not match `products`) and also caps the qualifier chain at the two
        // segments `ensure_namespace` can build -- a deeper name simply does not match, which is
        // the conservative outcome.
        let text = format!("{a} {b}").to_lowercase().replace('"', "");
        for key in schema.keys() {
            // A key is `name` or, for a name declared in several schemas, `schema.name`; the name is
            // what to look for, and `resolve` decides whether the spelling found means this table.
            let name = key.rsplit('.').next().unwrap_or(key);
            let re = regex::Regex::new(&format!(
                r"(?:^|[^\w.])((?:[a-z_]\w*\.){{0,2}}){}(?:[^\w]|$)",
                regex::escape(name)
            ))
            .unwrap();
            for caps in re.captures_iter(&text) {
                let mut parts: Vec<String> = caps[1]
                    .split('.')
                    .filter(|s| !s.is_empty())
                    .map(str::to_string)
                    .collect();
                parts.push(name.to_string());
                if resolve(schema, &parts) == Some(key) {
                    forms.entry(key.clone()).or_default().insert(parts);
                }
            }
        }
    }
    forms
}
