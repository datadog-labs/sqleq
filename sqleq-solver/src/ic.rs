// Unless explicitly stated otherwise all files in this repository are licensed under the
// Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/).
// Copyright 2026-Present Datadog, Inc.

//! Integrity constraints, and the rewrites that use them (SQLSolver's `QueryUExprICRewriter`).
//!
//! The constraints come from the IR's own schemas, which is exactly what Java sees: `IrDriver`
//! parses a DDL that the emitter derives from `ir.schemas` alone (`src/sqlsolver.rs`), with `NOT
//! NULL` for every non-nullable column and one `UNIQUE` per key group, dropping any group that has a
//! nullable column. A unique key whose columns are all NOT NULL is what makes a table a *set*: no two
//! rows share the key, so no row appears twice.

use std::collections::{HashMap, HashSet};

use crate::ir::Schema;

#[derive(Debug, Clone, Default)]
pub struct Ics {
    /// Table name -> its NOT NULL column indices.
    pub not_null: HashMap<String, HashSet<u32>>,
    /// Table name -> its unique keys, each a set of NOT NULL columns.
    pub keys: HashMap<String, Vec<Vec<u32>>>,
}

impl Ics {
    pub fn from_schemas(schemas: &[Schema]) -> Ics {
        let mut ics = Ics::default();
        for s in schemas {
            // An absent `nullable` list says nothing, so every column stays nullable.
            let not_null: HashSet<u32> =
                s.nullable.iter().enumerate().filter(|(_, n)| !**n).map(|(i, _)| i as u32).collect();
            let keys: Vec<Vec<u32>> = s
                .key
                .iter()
                .map(|k| k.iter().map(|c| *c as u32).collect::<Vec<u32>>())
                .filter(|k| !k.is_empty() && k.iter().all(|c| not_null.contains(c)))
                .collect();
            if !not_null.is_empty() {
                ics.not_null.insert(s.name.clone(), not_null);
            }
            if !keys.is_empty() {
                ics.keys.insert(s.name.clone(), keys);
            }
        }
        ics
    }

    pub fn is_empty(&self) -> bool {
        self.not_null.is_empty() && self.keys.is_empty()
    }

    /// A table with a unique NOT NULL key never holds the same row twice, so its membership
    /// indicator is 0/1.
    pub fn is_set(&self, table: &str) -> bool {
        self.keys.contains_key(table)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::Type;

    #[test]
    fn a_key_with_a_nullable_column_is_dropped() {
        let schemas = vec![Schema {
            name: "t".into(),
            types: vec![Type::Integer, Type::Integer],
            key: vec![vec![0], vec![1]],
            nullable: vec![false, true],
            opaque_identity: vec![],
        }];
        let ics = Ics::from_schemas(&schemas);
        assert_eq!(ics.keys.get("t"), Some(&vec![vec![0]]));
        assert_eq!(ics.not_null.get("t"), Some(&HashSet::from([0])));
        assert!(ics.is_set("t"));
    }
}
