// Unless explicitly stated otherwise all files in this repository are licensed under the
// Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/).
// Copyright 2026-Present Datadog, Inc.

//! How deep a parsed statement may nest before the frontend refuses it.
//!
//! The parser's recursion limit bounds what the parser builds by recursing, and nothing else. A
//! chain of one left-associative operator, `a + a + ... + a`, and a chain of set operations,
//! `SELECT .. UNION ALL SELECT .. UNION ALL ..`, are built by a loop instead, so a few thousand terms
//! parse into a tree a few thousand levels deep. Nearly every pass after the parse recurses on that
//! tree, lowering included, and the frontend ran out of stack and aborted on one.
//!
//! [`check`] runs straight after the parse, before any of those passes, and refuses a statement
//! nested deeper than [`MAX_DEPTH`]. Both of its walks are sqlparser's visitor, which grows its
//! stack as it descends, so neither can overflow on the trees they exist to catch.

use std::ops::ControlFlow;

use sqlparser::ast::{Expr, Query, SetExpr, Statement, Value, Values, Visit, VisitMut, Visitor, VisitorMut};

use crate::error::{unsupported, Result};

/// The parser's recursion limit, and the deepest nesting [`check`] accepts.
pub const MAX_DEPTH: usize = 1024;

/// Refuse `statements` if any of them nests deeper than [`MAX_DEPTH`].
///
/// The depth of a path through a statement counts its expressions, a subquery's included, and the
/// set operations of every query body it passes through. A refused tree is taken apart before it is
/// handed back, because the derived `Drop` recurses once per level as well: a chain of a few hundred
/// thousand terms would overflow the stack in the drop that follows the refusal.
pub fn check(statements: &mut [Statement]) -> Result<()> {
    if statements.iter().any(|st| st.visit(&mut Depth(0)).is_break()) {
        for st in statements.iter_mut() {
            let _ = st.visit(&mut Dismantle);
        }
        return Err(unsupported(format!("expression or set operation nested more than {MAX_DEPTH} levels deep")));
    }
    Ok(())
}

/// The nesting so far on the path being visited.
struct Depth(usize);

impl Depth {
    fn enter(&mut self, levels: usize) -> ControlFlow<()> {
        self.0 += levels;
        if self.0 > MAX_DEPTH {
            return ControlFlow::Break(());
        }
        ControlFlow::Continue(())
    }
}

impl Visitor for Depth {
    type Break = ();

    fn pre_visit_expr(&mut self, _: &Expr) -> ControlFlow<()> {
        self.enter(1)
    }

    fn post_visit_expr(&mut self, _: &Expr) -> ControlFlow<()> {
        self.0 -= 1;
        ControlFlow::Continue(())
    }

    /// The set operations of a query body are not expressions, so they are counted here: everything
    /// under the query is charged the depth of its deepest set-operation branch.
    fn pre_visit_query(&mut self, q: &Query) -> ControlFlow<()> {
        self.enter(set_depth(&q.body))
    }

    fn post_visit_query(&mut self, q: &Query) -> ControlFlow<()> {
        self.0 -= set_depth(&q.body);
        ControlFlow::Continue(())
    }
}

/// The depth of the deepest set-operation branch of a query body, iteratively.
fn set_depth(body: &SetExpr) -> usize {
    let mut deepest = 0;
    let mut todo = vec![(body, 0)];
    while let Some((b, d)) = todo.pop() {
        deepest = deepest.max(d);
        if let SetExpr::SetOperation { left, right, .. } = b {
            todo.extend([(&**left, d + 1), (&**right, d + 1)]);
        }
    }
    deepest
}

/// Replaces every expression and every set operation with a leaf, bottom-up, so that each node is
/// dropped once its children already are leaves.
struct Dismantle;

impl VisitorMut for Dismantle {
    type Break = ();

    fn post_visit_expr(&mut self, e: &mut Expr) -> ControlFlow<()> {
        *e = Expr::Value(Value::Null.with_empty_span());
        ControlFlow::Continue(())
    }

    fn post_visit_query(&mut self, q: &mut Query) -> ControlFlow<()> {
        let leaf = SetExpr::Values(Values { explicit_row: false, value_keyword: false, rows: Vec::new() });
        let mut todo = vec![std::mem::replace(&mut *q.body, leaf)];
        while let Some(b) = todo.pop() {
            if let SetExpr::SetOperation { left, right, .. } = b {
                todo.extend([*left, *right]);
            }
        }
        ControlFlow::Continue(())
    }
}
