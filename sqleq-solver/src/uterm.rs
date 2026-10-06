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

/// A constant. Two constants are the same value only when they are the same value *of the same
/// type*: the integer `1` and the decimal `1.0` compare equal under SQL's `=`, but `CAST(1 AS
/// TEXT)` is `'1'` and `CAST(1.0 AS TEXT)` is `'1.0'`, so wherever a function of a constant is
/// taken the two must stay apart. Likewise a decimal keeps its scale: `1.0` and `1.00` are equal
/// numbers and different values. [`UConst::same_value`] is that identity, [`UConst::number`] the
/// exact numeric reading that only a comparison may use.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum UConst {
    /// An INTEGER value.
    Int(i64),
    /// A REAL (`numeric`) value, as the canonical spelling [`UConst::decimal`] gives it: its exact
    /// value with exactly its scale's fractional digits, so equal spellings are equal values.
    /// Never an integer, even when integral: `2.0` is not `2`.
    Decimal(String),
    Str(String),
    /// An explicit, unambiguous NULL sentinel -- see the module doc for why this replaces Java's two
    /// inconsistent encodings instead of reusing either.
    Null,
}

impl UConst {
    /// The REAL constant a numeric literal spells, in canonical form (`01.50` and `1.50` give one
    /// constant, `1.5` another), or `None` when the text is not a decimal number Postgres would read
    /// exactly (`NaN`, `Infinity`, or no digits at all).
    pub fn decimal(text: &str) -> Option<UConst> {
        let (n, scale) = Number::parse_with_scale(text)?;
        Some(UConst::Decimal(n.spell(scale)))
    }

    /// The exact numeric value of an INTEGER or REAL constant.
    pub fn number(&self) -> Option<Number> {
        match self {
            UConst::Int(n) => Some(Number::from_i64(*n)),
            UConst::Decimal(s) => Number::parse_with_scale(s).map(|(n, _)| n),
            UConst::Str(_) | UConst::Null => None,
        }
    }

    /// Whether the two are one value: `Some(true)` when they are the same value of the same type,
    /// `Some(false)` when they differ under any reading (two different numbers, two different
    /// strings, NULL against anything else, a number against a string), and `None` when neither
    /// can be said -- two equal numbers of different types or scales (`1`, `1.0`, `1.00`), which
    /// are equal under SQL's `=` and not interchangeable, or a decimal that does not parse.
    pub fn same_value(&self, other: &UConst) -> Option<bool> {
        match (self, other) {
            (UConst::Int(a), UConst::Int(b)) => Some(a == b),
            (UConst::Str(a), UConst::Str(b)) => Some(a == b),
            (UConst::Null, UConst::Null) => Some(true),
            (UConst::Null, _) | (_, UConst::Null) => Some(false),
            (UConst::Str(_), _) | (_, UConst::Str(_)) => Some(false),
            (a, b) => match (a.number(), b.number()) {
                (Some(x), Some(y)) if x != y => Some(false),
                _ => (a == b).then_some(true),
            },
        }
    }

    /// The constant that stands for this one's numeric value alone: an integer when the value is
    /// integral and fits, otherwise the decimal with no trailing zeros. Only a position that
    /// depends on nothing but the value (the operand of SQL's numeric `=`) may use it.
    pub fn numeric_canonical(&self) -> Option<UConst> {
        let n = self.number()?;
        Some(match n.to_i64() {
            Some(i) => UConst::Int(i),
            None => UConst::Decimal(n.spell(0)),
        })
    }
}

/// An exact decimal number: `±0.d₁d₂… × 10^exp`, with `digits` free of leading and trailing zeros
/// (empty for zero, which is never negative). Two numbers are equal exactly when their fields are,
/// and they order as the numbers do. Nothing goes through a float.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Number {
    negative: bool,
    digits: String,
    exp: i64,
}

impl Number {
    pub fn from_i64(n: i64) -> Number {
        Number::from_parts(n < 0, &n.unsigned_abs().to_string(), 0)
    }

    /// `±digits × 10^shift`, normalized.
    fn from_parts(negative: bool, digits: &str, shift: i64) -> Number {
        let unled = digits.trim_start_matches('0');
        let lead = (digits.len() - unled.len()) as i64;
        let significant = unled.trim_end_matches('0');
        if significant.is_empty() {
            return Number { negative: false, digits: String::new(), exp: 0 };
        }
        // `digits` read as an integer is `0.digits × 10^len`; each leading zero moves the point one
        // place left, and trailing zeros change nothing after the point.
        Number { negative, digits: significant.to_string(), exp: digits.len() as i64 + shift - lead }
    }

    /// A numeric literal as Postgres reads it (`[+-]digits[.digits][e[+-]digits]`, at least one
    /// digit before the exponent), with the display scale Postgres gives it: the digits written
    /// after the point less the exponent, never below 0 (`1.50` has scale 2, `1.5e1` scale 0).
    /// `None` for anything else, and past the range Postgres's `numeric` accepts.
    pub fn parse_with_scale(text: &str) -> Option<(Number, i64)> {
        let (negative, rest) = match text.as_bytes().first()? {
            b'-' => (true, &text[1..]),
            b'+' => (false, &text[1..]),
            _ => (false, text),
        };
        let (mantissa, exponent) = match rest.find(['e', 'E']) {
            Some(i) => (&rest[..i], Some(&rest[i + 1..])),
            None => (rest, None),
        };
        let (int, frac) = mantissa.split_once('.').unwrap_or((mantissa, ""));
        let all_digits = |s: &str| s.bytes().all(|b| b.is_ascii_digit());
        if int.is_empty() && frac.is_empty() || !all_digits(int) || !all_digits(frac) {
            return None;
        }
        let e: i64 = match exponent {
            None => 0,
            Some(x) => {
                let unsigned = x.strip_prefix(['+', '-']).unwrap_or(x);
                if unsigned.is_empty() || !all_digits(unsigned) || unsigned.len() > 9 {
                    return None;
                }
                x.parse().ok()?
            }
        };
        let n = Number::from_parts(negative, &format!("{int}{frac}"), e - frac.len() as i64);
        let scale = (frac.len() as i64 - e).max(0);
        // `numeric` holds up to 131072 digits before the point and 16383 after.
        (scale <= 16_383 && n.exp <= 131_072).then_some((n, scale))
    }

    /// The value spelled with at least `scale` digits after the point (more if it needs them), no
    /// leading zeros, and no sign on zero.
    pub fn spell(&self, scale: i64) -> String {
        let len = self.digits.len() as i64;
        let frac_len = scale.max(len - self.exp).max(0) as usize;
        let int: String = if self.exp <= 0 {
            "0".to_string()
        } else {
            let take = self.exp.min(len) as usize;
            format!("{}{}", &self.digits[..take], "0".repeat((self.exp - take as i64) as usize))
        };
        let mut frac: String = if self.exp >= len {
            String::new()
        } else if self.exp >= 0 {
            self.digits[self.exp as usize..].to_string()
        } else {
            format!("{}{}", "0".repeat((-self.exp) as usize), self.digits)
        };
        frac.push_str(&"0".repeat(frac_len - frac.len()));
        let sign = if self.negative { "-" } else { "" };
        if frac.is_empty() {
            format!("{sign}{int}")
        } else {
            format!("{sign}{int}.{frac}")
        }
    }

    /// The value as an `i64`, when it is an integer in range.
    pub fn to_i64(&self) -> Option<i64> {
        if self.digits.is_empty() {
            return Some(0);
        }
        if self.exp < self.digits.len() as i64 || self.exp > 19 {
            return None;
        }
        self.spell(0).parse().ok()
    }
}

impl Ord for Number {
    fn cmp(&self, other: &Number) -> std::cmp::Ordering {
        let sign = |n: &Number| if n.digits.is_empty() { 0 } else if n.negative { -1 } else { 1 };
        match sign(self).cmp(&sign(other)) {
            std::cmp::Ordering::Equal if sign(self) == 0 => std::cmp::Ordering::Equal,
            std::cmp::Ordering::Equal => {
                // Same sign, both non-zero: the larger exponent is the larger magnitude, and with
                // equal exponents digit strings (no trailing zeros) compare as the fractions do.
                let magnitude = self.exp.cmp(&other.exp).then_with(|| self.digits.cmp(&other.digits));
                if self.negative {
                    magnitude.reverse()
                } else {
                    magnitude
                }
            }
            unequal => unequal,
        }
    }
}

impl PartialOrd for Number {
    fn partial_cmp(&self, other: &Number) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
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
    /// `[x = 0]`: 1 where `x` is 0 and 0 everywhere else, for any multiplicity `x` (a non-negative
    /// count), as `eval` and rung 3 read it. It is the complement `1 - x` only where `x` is 0/1;
    /// normalization relies on the general reading -- it pushes a negation into a sum of counts
    /// (`¬(a + b)` is `¬a · ¬b`) and reads `¬Σ_v b` as "no row satisfies `b`".
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

    mod exact_constants {
        use super::*;

        fn number(text: &str) -> Number {
            Number::parse_with_scale(text).expect(text).0
        }

        #[test]
        fn a_decimal_keeps_the_scale_postgres_gives_it() {
            let dec = |t: &str| UConst::decimal(t).expect(t);
            assert_eq!(dec("1.50"), UConst::Decimal("1.50".into()));
            assert_eq!(dec("+01.50"), UConst::Decimal("1.50".into()));
            assert_eq!(dec("-0.00"), UConst::Decimal("0.00".into()));
            assert_eq!(dec(".5"), UConst::Decimal("0.5".into()));
            assert_eq!(dec("1.5e1"), UConst::Decimal("15".into()));
            assert_eq!(dec("1.50e1"), UConst::Decimal("15.0".into()));
            assert_eq!(dec("1.5e-3"), UConst::Decimal("0.0015".into()));
            assert_eq!(dec("2E2"), UConst::Decimal("200".into()));
            for bad in ["", ".", "-", "e5", "1e", "1e+", "NaN", "inf", "Infinity", "1.2.3", "0x10", " 1", "1e9999999999"] {
                assert_eq!(UConst::decimal(bad), None, "{bad:?}");
            }
        }

        #[test]
        fn numbers_compare_exactly() {
            assert!(number("0.10000000000000001") > number("0.1"));
            assert_ne!(Number::from_i64(9_007_199_254_740_993), Number::from_i64(9_007_199_254_740_992));
            let ascending = ["-2", "-1.5", "-0.001", "0", "0.001", "0.0011", "1", "1.5", "10", "100.5"];
            for w in ascending.windows(2) {
                assert!(number(w[0]) < number(w[1]), "{} < {}", w[0], w[1]);
            }
            assert_eq!(number("2.00"), Number::from_i64(2));
            assert_eq!(number("-0.0"), Number::from_i64(0));
            assert_eq!(number("1e3"), Number::from_i64(1000));
            for n in [i64::MIN, -1, 0, 1, i64::MAX] {
                assert_eq!(Number::from_i64(n).to_i64(), Some(n));
            }
            assert_eq!(number("1.5").to_i64(), None);
            assert_eq!(number("99999999999999999999").to_i64(), None);
        }

        #[test]
        fn equal_numbers_of_different_types_or_scales_are_neither_one_value_nor_two() {
            let dec = |t: &str| UConst::decimal(t).unwrap();
            assert_eq!(UConst::Int(1).same_value(&dec("1.0")), None);
            assert_eq!(dec("1.0").same_value(&dec("1.00")), None);
            assert_eq!(dec("1.50").same_value(&dec("01.50")), Some(true));
            assert_eq!(UConst::Int(1).same_value(&UConst::Int(2)), Some(false));
            assert_eq!(dec("1.5").same_value(&UConst::Int(1)), Some(false));
            assert_eq!(UConst::Str("a".into()).same_value(&UConst::Str("a".into())), Some(true));
            assert_eq!(UConst::Null.same_value(&UConst::Int(0)), Some(false));
            assert_eq!(UConst::Str("1".into()).same_value(&UConst::Int(1)), Some(false));
        }

        #[test]
        fn the_numeric_canonical_constant_depends_on_the_value_alone() {
            let dec = |t: &str| UConst::decimal(t).unwrap();
            assert_eq!(dec("2.00").numeric_canonical(), Some(UConst::Int(2)));
            assert_eq!(dec("2.50").numeric_canonical(), Some(UConst::Decimal("2.5".into())));
            assert_eq!(dec("99999999999999999999.0").numeric_canonical(), Some(UConst::Decimal("99999999999999999999".into())));
            assert_eq!(UConst::Str("2".into()).numeric_canonical(), None);
        }
    }
}
