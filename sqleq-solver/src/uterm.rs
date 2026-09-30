// Unless explicitly stated otherwise all files in this repository are licensed under the
// Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/).
// Copyright 2026-Present Datadog, Inc.

//! The U-expression term algebra (SQLSolver's `uexpr` Java package), reimplemented as one enum per
//! concept instead of that package's `UXxx`/`UXxxImpl` class pairs.
//!
//! `UConst::Null` is a first-class, explicit sentinel rather than Java's two mutually-inconsistent
//! encodings (`Integer.MIN_VALUE` for `UConst`, empty-string for `UString`). Unlike Java, nothing here
//! assumes `Null` shares a domain with real values by construction -- see `translate.rs`'s `Value`
//! type and its `value_eq` helper for how a value that might be null is actually bound to a column,
//! which is the only place `Null` needs to behave like a value at all.

use std::rc::Rc;

/// A row variable. `Base` is a fresh, opaque row binder (existentially quantified, or a subquery's
/// bound variable); `Proj` extracts one column out of a `Base` row by position (our IR is already
/// positionally/de-Bruijn resolved, so this carries an index, not Java's attribute name).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum UVar {
    Base(u32),
    Proj { index: u32, base: Box<UVar> },
}

impl UVar {
    /// This var with every occurrence of base var `from` renamed to `to`.
    pub fn rename_base(&self, from: u32, to: u32) -> UVar {
        match self {
            UVar::Base(id) if *id == from => UVar::Base(to),
            UVar::Base(_) => self.clone(),
            UVar::Proj { index, base } => UVar::Proj { index: *index, base: Box::new(base.rename_base(from, to)) },
        }
    }

    /// Smart constructor mirroring `UExprConcreteTranslator.mkProjVar`: projecting through an
    /// existing `Proj` re-targets the *inner* base rather than nesting (case 2 of `mkProjVar` in the
    /// Java translator), so a `Proj` in this port is always ultimately backed by a `Base`, never by
    /// another `Proj`.
    pub fn proj(index: u32, base: UVar) -> UVar {
        match base {
            UVar::Proj { base: inner, .. } => UVar::Proj { index, base: inner },
            other => UVar::Proj { index, base: Box::new(other) },
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum UConst {
    Int(i64),
    /// A non-integral numeric literal, carried as its exact source text rather than a Java-style
    /// "opaque zero-arg function named after its own string value" trick.
    Decimal(String),
    Str(String),
    /// An explicit, unambiguous NULL sentinel -- see the module doc for why this replaces Java's two
    /// inconsistent encodings instead of reusing either.
    Null,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PredKind {
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
}

/// One U-expression term. `Add`/`Mul` are n-ary and flat by construction (via `mk_add`/`mk_mul`,
/// mirroring `UAdd`/`UMul`'s one-level auto-flattening); `Sum`'s body is always structurally `Add` or
/// `Mul` (via `mk_sum`, mirroring `USum`'s invariant, "enforced only by the smart constructor, not by
/// the type itself" in the Java code -- same caveat applies here since nothing stops a caller from
/// hand-building `UTerm::Sum` directly).
///
/// `Add`/`Mul`/`Squash`/`Neg`/`Sum` hold their children behind `Rc` rather than owning them
/// directly. In Java, reusing the same subterm object at several call sites (e.g. `JOIN`'s
/// null-padding branch reusing the matched-rows term) costs nothing -- it's just another
/// reference to the same object graph. A plain owned Rust tree has no such sharing, so the same
/// reuse pattern deep-clones the whole subtree at every reuse site; chained through even a few
/// dozen `LEFT JOIN`s (each reusing its own two branches' terms 2-3 times) that compounds to
/// `O(2^n)` and exhausts memory long before it panics -- observed on real input, not a
/// hypothetical. `Rc` makes every such reuse an
/// O(1) refcount bump instead, restoring Java's sharing behavior. `Pred`/`Func`'s `args` stay
/// plain `Vec<UTerm>`: they're always leaf-level comparisons/functions over already-small
/// operands (`Var`/`Const`/a `Value`'s fields), and by the time one is reused it's already
/// wrapped inside an `Add`/`Mul` via `mk_add`/`mk_mul`, which is where the sharing actually needs
/// to happen.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum UTerm {
    Const(UConst),
    /// The scalar value of a `Proj` var.
    Var(UVar),
    /// A row-membership predicate: is this `var` a real row of table `name`?
    Table { name: String, var: UVar },
    /// One of the six canonical comparisons. `args.len() == 2` always.
    Pred { kind: PredKind, args: Vec<UTerm> },
    /// An opaque/uninterpreted function, used both for genuinely-uninterpreted scalar functions
    /// (`divide`, `concat`, `UPPER`, ...) and for boolean-valued opaque predicates (`like`) --
    /// deliberately not wrapped in a wrapping `UPred` the way Java's `[0 < like(...)]` does, since
    /// this `Func` variant isn't constrained to need boolean-typing help from a comparison.
    Func { name: String, args: Vec<UTerm> },
    Add(Vec<Rc<UTerm>>),
    Mul(Vec<Rc<UTerm>>),
    /// Clamp to exactly 0 or 1 (used for OR-of-indicators and set semantics).
    Squash(Rc<UTerm>),
    /// Boolean complement, `1 - x`, valid only where `x` is already 0/1-valued.
    Neg(Rc<UTerm>),
    /// Existential quantification over `vars` (always `Base`-kind, by construction of every call
    /// site in `translate.rs`; not enforced by this type any more than Java enforces it on `USum`).
    Sum { vars: Vec<UVar>, body: Rc<UTerm> },
}

impl UTerm {
    /// Node count of this term read as a tree, i.e. with every `Rc`-shared child counted once per
    /// occurrence -- the cost any non-memoizing recursive pass pays. Saturates at `cap`, and memoizes
    /// by node address so computing it is linear in the shared (DAG) size even when the tree size is
    /// exponential, which is exactly the case it exists to detect.
    pub fn tree_size(&self, cap: usize) -> usize {
        fn go(t: &UTerm, cap: usize, memo: &mut std::collections::HashMap<*const UTerm, usize>) -> usize {
            if let Some(&n) = memo.get(&(t as *const UTerm)) {
                return n;
            }
            let mut n = 1usize;
            let add = |child: &UTerm, n: &mut usize, memo: &mut std::collections::HashMap<*const UTerm, usize>| {
                *n = n.saturating_add(go(child, cap, memo)).min(cap);
            };
            match t {
                UTerm::Const(_) | UTerm::Var(_) | UTerm::Table { .. } => {}
                UTerm::Pred { args, .. } | UTerm::Func { args, .. } => args.iter().for_each(|a| add(a, &mut n, memo)),
                UTerm::Add(ts) | UTerm::Mul(ts) => ts.iter().for_each(|c| add(c, &mut n, memo)),
                UTerm::Squash(c) | UTerm::Neg(c) | UTerm::Sum { body: c, .. } => add(c, &mut n, memo),
            }
            memo.insert(t as *const UTerm, n);
            n
        }
        go(self, cap, &mut std::collections::HashMap::new())
    }

    /// This term with base var `from` renamed to `to` everywhere, binders included. Memoized by
    /// `Rc` address, so it is linear in the shared size and the result keeps the input's sharing.
    pub fn rename_base(&self, from: u32, to: u32) -> UTerm {
        type Memo = std::collections::HashMap<*const UTerm, Rc<UTerm>>;
        fn rc(c: &Rc<UTerm>, from: u32, to: u32, memo: &mut Memo) -> Rc<UTerm> {
            if let Some(done) = memo.get(&Rc::as_ptr(c)) {
                return done.clone();
            }
            let done = Rc::new(go(c, from, to, memo));
            memo.insert(Rc::as_ptr(c), done.clone());
            done
        }
        fn go(t: &UTerm, from: u32, to: u32, memo: &mut Memo) -> UTerm {
            match t {
                UTerm::Const(_) => t.clone(),
                UTerm::Var(v) => UTerm::Var(v.rename_base(from, to)),
                UTerm::Table { name, var } => UTerm::Table { name: name.clone(), var: var.rename_base(from, to) },
                UTerm::Pred { kind, args } => {
                    UTerm::Pred { kind: *kind, args: args.iter().map(|a| go(a, from, to, memo)).collect() }
                }
                UTerm::Func { name, args } => {
                    UTerm::Func { name: name.clone(), args: args.iter().map(|a| go(a, from, to, memo)).collect() }
                }
                UTerm::Add(ts) => UTerm::Add(ts.iter().map(|c| rc(c, from, to, memo)).collect()),
                UTerm::Mul(ts) => UTerm::Mul(ts.iter().map(|c| rc(c, from, to, memo)).collect()),
                UTerm::Squash(c) => UTerm::Squash(rc(c, from, to, memo)),
                UTerm::Neg(c) => UTerm::Neg(rc(c, from, to, memo)),
                UTerm::Sum { vars, body } => UTerm::Sum {
                    vars: vars.iter().map(|v| v.rename_base(from, to)).collect(),
                    body: rc(body, from, to, memo),
                },
            }
        }
        go(self, from, to, &mut Memo::new())
    }
}

/// Flattens one level: an operand that's already `Add` splices its own terms in directly, so no
/// operand ever holds a nested `Add` (mirrors `UAdd.mk`). Empty input is the additive identity.
/// Operands are `Rc`-wrapped as they're collected, so the singleton case (`flat.len() == 1`)
/// clones only the top-level node, not its (now `Rc`-shared) children.
pub fn mk_add(terms: impl IntoIterator<Item = UTerm>) -> UTerm {
    let mut flat: Vec<Rc<UTerm>> = Vec::new();
    for t in terms {
        match t {
            UTerm::Add(inner) => flat.extend(inner),
            other => flat.push(Rc::new(other)),
        }
    }
    match flat.len() {
        0 => UTerm::Const(UConst::Int(0)),
        1 => (*flat.pop().expect("len == 1")).clone(),
        _ => UTerm::Add(flat),
    }
}

/// Flattens one level, mirroring `UMul.mk`. Empty input is the multiplicative identity.
pub fn mk_mul(terms: impl IntoIterator<Item = UTerm>) -> UTerm {
    let mut flat: Vec<Rc<UTerm>> = Vec::new();
    for t in terms {
        match t {
            UTerm::Mul(inner) => flat.extend(inner),
            other => flat.push(Rc::new(other)),
        }
    }
    match flat.len() {
        0 => UTerm::Const(UConst::Int(1)),
        1 => (*flat.pop().expect("len == 1")).clone(),
        _ => UTerm::Mul(flat),
    }
}

/// Wraps `body` in a singleton `Mul` if it isn't already structurally `Add`/`Mul`, mirroring
/// `USum.mk`'s invariant. Binding an empty var list degenerates to the body itself (nothing to
/// existentially quantify over).
pub fn mk_sum(vars: Vec<UVar>, body: UTerm) -> UTerm {
    if vars.is_empty() {
        return body;
    }
    let body = match body {
        UTerm::Add(_) | UTerm::Mul(_) => body,
        other => mk_mul([other]),
    };
    UTerm::Sum { vars, body: Rc::new(body) }
}

pub fn mk_squash(t: UTerm) -> UTerm {
    UTerm::Squash(Rc::new(t))
}

pub fn mk_neg(t: UTerm) -> UTerm {
    UTerm::Neg(Rc::new(t))
}

/// Boolean OR of two 0/1-valued terms: `Squash(x + y)`. Plain addition alone would double-count when
/// both are 1 (a latent multiplicity bug in Java's `mkIsNullPred`); squashing here
/// avoids reproducing it.
pub fn mk_or(a: UTerm, b: UTerm) -> UTerm {
    mk_squash(mk_add([a, b]))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn proj_of_proj_collapses_to_the_inner_base() {
        let base = UVar::Base(0);
        let once = UVar::proj(1, base);
        let twice = UVar::proj(2, once);
        assert_eq!(twice, UVar::Proj { index: 2, base: Box::new(UVar::Base(0)) });
    }

    #[test]
    fn mk_add_flattens_one_level_and_collapses_singletons() {
        let a = mk_add([UTerm::Const(UConst::Int(1)), UTerm::Const(UConst::Int(2))]);
        let b = mk_add([a, UTerm::Const(UConst::Int(3))]);
        assert_eq!(
            b,
            UTerm::Add(vec![
                Rc::new(UTerm::Const(UConst::Int(1))),
                Rc::new(UTerm::Const(UConst::Int(2))),
                Rc::new(UTerm::Const(UConst::Int(3))),
            ])
        );
        assert_eq!(mk_add([UTerm::Const(UConst::Int(5))]), UTerm::Const(UConst::Int(5)));
        assert_eq!(mk_add(std::iter::empty()), UTerm::Const(UConst::Int(0)));
    }

    #[test]
    fn mk_sum_wraps_a_bare_body_in_a_singleton_mul() {
        let v = UVar::Base(0);
        let sum = mk_sum(vec![v.clone()], UTerm::Var(v.clone()));
        assert_eq!(sum, UTerm::Sum { vars: vec![v.clone()], body: Rc::new(UTerm::Var(v)) });
    }
}
