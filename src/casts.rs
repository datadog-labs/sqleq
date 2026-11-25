// Unless explicitly stated otherwise all files in this repository are licensed under the
// Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/).
// Copyright 2026-Present Datadog, Inc.

//! What to do with a cast the query writes down.
//!
//! [`infer`](crate::infer) decides what type everything *has*. This module decides what to do with
//! the places the query states a type outright — every `CAST(x AS T)` / `x::T` — which is the last
//! thing standing between the SQL of an input pair and the IR the prover reads.
//!
//! ## The five rules
//!
//! | | operand | becomes | why it is faithful |
//! |---|---|---|---|
//! | 1 | `$N` | `$N` | the parameter *is* an uninterpreted constant of the target type, so the cast is the identity on it. Only where the target is the type Postgres gave `$N`: see below |
//! | 2/3 | a literal or `NULL` | `CAST(lit AS T′)` | a real cast survives; `T′` is only the prover's spelling of `T` |
//! | 4 | anything already of type `T`, target unqualified | the operand | a no-op cast |
//! | 5a | `INTEGER` → `REAL` | `CAST(x AS NUMERIC)` | a widening the prover models natively |
//! | 5b | any other real conversion | `qcastK(x)` | an uninterpreted function, shared across both sides |
//!
//! Rule 4's *unqualified* condition is load-bearing: `x::varchar` over a VARCHAR column is a no-op,
//! but `x::varchar(8)` is a truncation and must not be dropped. [`map_type_name`] reports the
//! qualifier separately for exactly this.
//!
//! ## When rule 1 holds
//!
//! `$N::T` is the identity exactly when Postgres gave `$N` the type `T`. Postgres types an untyped
//! parameter at its first use and converts at every later cast to another type, so in
//! `n = ANY($1::int[]) AND name = ANY($1::text[])` the parameter is an `int[]`, and with the two
//! conjuncts the other way round it is a `text[]`: two statements that differ.
//!
//! So the rule asks every use of the parameter, in both queries, what type it states
//! ([`stated_types`]), and drops the cast only where each of them is a use it reads and states the
//! cast's type. That holds whatever the type is: one inference has a class for, where its own
//! test (the cast must state the type the parameter was inferred to have) is kept as well, or one
//! it has none for (an array, `uuid`, `jsonb`, a float, a range, an enum).
//!
//! * **A use that states another type** makes the cast a conversion. In `ts = $1::date` beside
//!   `ts = $1` over a `timestamp` column the parameter is a `date` in one statement and a
//!   `timestamp` in the other, and the cast truncates what the bare one holds. Inference keeps the
//!   temporal classes apart in a comparison, so its test does not see this.
//! * **A use that states none** (an argument of a function, an operand of arithmetic, a branch of
//!   a `CASE`) leaves the parameter's type to an order of reading the frontend does not follow:
//!   `abs($1)` makes `$1` a `double precision`, which `$1::int` rounds. Inference reads no type
//!   off such a use, so there too its test passes where the cast converts.
//!
//! The uses that are read are the ones Postgres has a plain rule for: a cast; a comparison with a
//! column, a cast or a constant, a `BETWEEN`, an `IN` list or a `LIKE` over one; a `LIMIT` count;
//! a value stored in a column, where the DDL says what the column is; and a bare item of a select
//! list, which is a `text`, or in a set operation what the other branch's item is. A typmod is no
//! part of the type a use states, and a cast that carries one (`$1::bit(3)`) is a computation
//! whatever its uses say, which rule 5b keeps.
//!
//! ## Why rule 5b cannot license a false proof
//!
//! `qcastK` is a fresh uninterpreted function symbol `f : S → T`, standing in for the real
//! conversion `g`. The prover quantifies over every interpretation of `f`, and `f = g` is one of
//! them — so a proof that holds for all `f` holds for `g`. That argument does not depend on which
//! occurrences share a symbol, which means **both sharing and splitting are sound** and the dedup
//! key only trades completeness: sharing lets the two sides cancel a conversion against each other,
//! splitting stops them. The preprocessor keys on `(target type, operand text)`, so this does too.
//!
//! One consequence worth naming, because it is a quirk rather than a design: the key omits the
//! *argument* type, so two operands that render identically but were inferred differently collide,
//! and the declaration records whichever arrived first. Faithful to the original, and harmless — a
//! wrong argument sort makes the symbol apply to a different domain, not the conversion unsound.
//!
//! ## Mutating a tree that identity maps point into
//!
//! Inference keys everything on [`nid`] — a node's address, standing in for Python's `id(node)`.
//! Python can rewrite freely because its objects never move; Rust's do. Two properties keep the keys
//! valid here:
//!
//! * **Decide first, then apply.** [`decide`] reads the untouched tree and records one [`Decision`]
//!   per cast. Nothing in that pass depends on another cast having been rewritten already — the
//!   preprocessor's decisions are order-independent too — so splitting them costs nothing.
//! * **Apply bottom-up, in place.** [`Apply`] fires on `post_visit_expr`, so a node is rewritten
//!   only after its descendants, and it *overwrites* `*e` rather than replacing the parent's `Box`.
//!   An ancestor therefore never moves while its descendants are being rewritten, and the
//!   decision keys stay live for the whole pass.
//!
//! What does move is a hoisted operand (rules 1 and 4), from wherever it lived into the cast's slot.
//! Only that one node changes address — its own children sit behind `Box`/`Vec` indirections whose
//! heap buffers do not move — so re-keying the single entry restores the invariant. [`Apply`]
//! collects those and [`rewrite_casts`] applies them.
//!
//! Rule 5b is the mirror image: the preprocessor wraps a `.copy()` of the operand, which in Python
//! means every `id()` inside it becomes fresh and every map built before the rewrite stops
//! resolving there. Moving the subtree in Rust invalidates only its root. The remaining addresses
//! are dropped explicitly so that the port sees the same thing the original does.
//!
//! ### Cast over cast
//!
//! A cast can be the direct operand of another cast, so both invariants have to hold for a chain.
//! They do, and one consequence is worth stating because it is load-bearing rather than incidental:
//!
//! * Bottom-up means the inner cast is rewritten into its own slot before the outer is visited, so
//!   the outer still finds an `Expr::Cast` there and re-reads the rewritten operand. The outer's
//!   *decision* was taken from the inner's declared `data_type`, not from the inner's decision, so
//!   the two passes stay independent.
//! * `let mut op = *expr` in [`Apply`] does free a `Box` mid-traversal, and a later allocation can
//!   land on that address. It can never resurrect a live key: bottom-up guarantees the freed box
//!   belonged to a *descendant*, whose decision was already consumed.
//! * Two chained hoists push `(inner operand, inner slot)` and then `(inner slot, outer slot)`.
//!   [`rewrite_casts`] replays `remap` in push order, so the entry walks the chain to its final
//!   home. **The order is the correctness argument**, not a coincidence of the data structure.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::ops::ControlFlow;

use sqlparser::ast::{
    visit_expressions, CastKind, DataType, ExactNumberInfo, Expr, Function, FunctionArg,
    FunctionArgExpr, FunctionArgumentList, FunctionArguments, Ident, ObjectName, ObjectNamePart, Query,
    SetExpr, Statement, TimezoneInfo, Value as SqlValue, VisitMut, Visitor, VisitorMut,
};

use crate::catalog::{obj_name, Catalog, FnDecl};
use crate::error::{unsupported, FrontendError, Result};
use crate::infer::{
    builtin_rtype, canon_type_name, is_agg_name, map_type_name, nid, sweep, Atom, Inferred, Ty, Uf,
};

/// A shared uninterpreted conversion: rule 5b's `qcastK`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct QCast {
    /// `qcast0`, `qcast1`, … — numbered in the order the rewrite first needed them.
    pub name: String,
    /// The cast's target type: the symbol's declared return type.
    pub ret: Ty,
}

/// What the rewrite produced.
#[derive(Default, Debug)]
pub struct Rewrite {
    pub qcasts: Vec<QCast>,
    /// The refusal of a cast over a parameter whose uses do not all state the cast's type, held
    /// and not raised. The two queries' `$N` are one parameter only if the pair numbers its
    /// parameters alike, and where it does not, two uses that disagree are two parameters: the
    /// misalignment is then the pair's refusal (`params`), and this one its artifact. The caller
    /// raises it once it knows the pair is aligned. The tree is rewritten as if the uses agreed,
    /// which is what the alignment check reads.
    pub mistyped: Option<FrontendError>,
}

/// What the cast rewrite reads beside the queries and what inference made of them.
#[derive(Clone, Copy, Default)]
pub struct Context<'a> {
    /// The catalog the DDL declares, where there is one to seed from. It says what type a column a
    /// parameter is compared with has.
    pub declared: Option<&'a Catalog>,
    /// Where each query, a reduced `INSERT` or `UPDATE`, stores its outputs.
    pub stores: Option<&'a [crate::dml::Stores; 2]>,
}

/// The type a use of a parameter states, as far as it settles what the parameter's type is.
#[derive(Clone, Debug, PartialEq, Eq)]
enum TypeKey {
    /// A type inference has a [`Ty`] for. The class is the key, whatever the spelling and the
    /// typmod: `bigint` and `integer` are one, as they are to [`canon_target`].
    Scalar(Ty),
    /// A type it has none for, by its name as Postgres folds it ([`type_name`]). Without a typmod:
    /// a parameter's type has none, so a `bit(3)` and a `bit(5)` column both make it a `bit`.
    Named(String),
    /// An array of the element's key. Postgres has one array type per element type, however many
    /// dimensions the spelling writes.
    Array(Box<TypeKey>),
}

/// The [`TypeKey`] of a type as a cast or the DDL spells it.
fn type_key(txt: &str) -> TypeKey {
    let folded = crate::types::fold_type_name(txt);
    let (elem, array) = array_element(&folded);
    let name = type_name(elem);
    let key = match map_type_name(&name) {
        (Some(ty), _) => TypeKey::Scalar(ty),
        (None, _) => TypeKey::Named(name),
    };
    if array {
        TypeKey::Array(Box::new(key))
    } else {
        key
    }
}

/// The name a [`TypeKey`] is made of, from a folded element spelling: what is left once everything
/// that does not tell two types apart is gone. Each step is one Postgres's own reading of a type
/// name takes, and nothing else is folded, because taking two types for one is the direction that
/// drops a cast it should not: a spelling this does not know keeps its own key, and a pair that
/// needs the two to be one type is refused.
///
/// * **A typmod goes.** `bit(3)`, `time(0) with time zone` and `geometry(Point,4326)` are the types
///   `bit`, `time with time zone` and `geometry`; what the modifier computes is [`has_typmod`]'s
///   business. `float(p)` is the one spelling whose parenthesis picks the type: `real` up to 24 and
///   `double precision` above.
/// * **Two names the grammar gives one type are one**: the floats, `time with time zone` and
///   `timetz`, `bit varying` and `varbit`.
/// * **`pg_catalog.T` is `T`** for a type Postgres predefines, which is where a bare `T` is looked
///   up first.
/// * **`public.T` is `T`** for a type it does not predefine and this crate gives no class: a
///   pair's enum or other user type, which the DDL declares in `public` and a cast names bare. This
///   takes the session's search path to find `public`'s type, as [`Catalog::resolve_bare`] does for
///   a table.
///
/// A quoted part keeps its quotes, and any other schema its name, so `"mood"` and `app.mood` are
/// keys of their own.
fn type_name(elem: &str) -> String {
    if let Some(p) = elem.strip_prefix("float(").and_then(|rest| rest.strip_suffix(')')) {
        return match p.trim().parse::<u32>() {
            Ok(1..=24) => "real".to_string(),
            Ok(25..=53) => "double precision".to_string(),
            _ => elem.to_string(),
        };
    }
    // Every `(..)` group outside double quotes, and the space it leaves beside another.
    let mut name = String::new();
    let (mut quoted, mut depth) = (false, 0usize);
    for c in elem.chars() {
        match c {
            '(' if !quoted => depth += 1,
            ')' if !quoted && depth > 0 => depth -= 1,
            _ if depth > 0 => {}
            ' ' if !quoted && (name.is_empty() || name.ends_with(' ')) => {}
            _ => {
                quoted ^= c == '"';
                name.push(c);
            }
        }
    }
    let name = name.trim_end();
    let unqualified = match name.split_once('.') {
        Some(("pg_catalog", t)) if crate::catalog::PG_CATALOG_TYPES.contains(&t) => t,
        Some(("public", t)) if user_type_name(t) => t,
        _ => name,
    };
    match unqualified {
        "float8" | "double precision" | "float" | "double" => "double precision",
        "float4" | "real" => "real",
        "time with time zone" => "timetz",
        "bit varying" => "varbit",
        other => other,
    }
    .to_string()
}

/// Whether a folded, unqualified name can only be a type the input's DDL creates: one unquoted
/// identifier that Postgres predefines no type under and that this crate reads as no class.
fn user_type_name(t: &str) -> bool {
    let ident = t.starts_with(|c: char| c.is_ascii_lowercase() || c == '_')
        && t.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '$');
    ident
        && !crate::catalog::PG_CATALOG_TYPES.contains(&t)
        && crate::types::scalar_class(t).is_none()
        && crate::types::temporal_class(&t.to_uppercase()).is_none()
        && crate::types::unfaithful_type(t).is_none()
}

/// A folded type spelling without what makes it an array, and whether anything did: its trailing
/// `[..]` groups, or the SQL-standard `T ARRAY` and `T ARRAY[n]`, which Postgres reads as `T[]`.
fn array_element(folded: &str) -> (&str, bool) {
    let mut elem = folded.trim_end();
    let mut array = false;
    while elem.ends_with(']') {
        let Some(open) = elem.rfind('[') else { break };
        elem = elem[..open].trim_end();
        array = true;
    }
    if let Some(of) = elem.strip_suffix(" array") {
        elem = of.trim_end();
        array = true;
    }
    (elem, array)
}

/// Whether a cast target computes something beyond naming a type: a length, a precision or an
/// interval's fields, on the type or on an array's element, whatever the type is. So `bit(3)` and
/// `timetz(0)` do, as `varchar(3)` does, and so does a type this crate does not know with anything
/// in parentheses. [`map_type_name`] reports no qualifier for an array, so `varchar(1)[]`, which
/// truncates each element, is asked about its element.
///
/// An unquoted `bit` with no length is `bit(1)` to Postgres's grammar, and truncates like one. The
/// quoted `"bit"` is the type with no length, and computes nothing.
fn has_typmod(txt: &str) -> bool {
    let folded = crate::types::fold_type_name(txt);
    let (elem, _) = array_element(&folded);
    map_type_name(elem).1 || elem == "bit"
}

/// What one use of a parameter says of its type.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Use {
    /// It states this type: the target of a cast over the parameter, or the declared type of the
    /// column it is compared with or stored in.
    Stated(TypeKey),
    /// It agrees with whatever the other uses state. A comparison with a column whose type no DDL
    /// declares and inference found nothing for: such a column has whatever type fits the pair.
    /// And `$N IS [NOT] NULL`, which types nothing: Postgres leaves the parameter to its other
    /// uses there, and does not prepare a statement in which that test comes first.
    Fits,
}

/// What the uses of one parameter state between them, in both queries.
#[derive(Clone, Debug, Default)]
struct Uses {
    /// The types its uses state, each once.
    keys: Vec<TypeKey>,
    /// Whether some use states no type.
    silent: bool,
}

impl Uses {
    /// Whether every use states `key`: none states another type and none is silent.
    fn all_state(&self, key: &TypeKey) -> bool {
        !self.silent && self.keys.as_slice() == std::slice::from_ref(key)
    }
}

/// For each parameter, what its uses state, in either query.
///
/// A use states a type when it is
/// * the operand of a cast: the target;
/// * compared by `=`, `<>`, `<`, `<=`, `>`, `>=` or `IS [NOT] DISTINCT FROM` with an operand whose
///   type is read: a bare column (its declared type), a cast (its target, so in `ts::date = $1` the
///   parameter is a `date`), `date(x)`, or a constant (a number's or a boolean's own type, and
///   `text` for a string, since Postgres compares two operands of unknown type as text). That is
///   the type Postgres resolves the parameter to there. Two rows are compared field by field, and
///   two bare parameters are `text` and tied ([`compared`]);
/// * a bound of such an operand's `BETWEEN`: the operand's type. Or the subject of a `BETWEEN`
///   whose two bounds are read as one type: theirs;
/// * an item of such an operand's `IN` list that no other item can widen: the operand's type
///   ([`in_list`]). The parameters of one list are held to one type between them ([`pool`]). Or
///   the subject of an `IN` list whose items are all read as one type: theirs;
/// * the pattern of a `LIKE`, `ILIKE` or `SIMILAR TO` over an operand read as `text`, or the
///   operand of one whose pattern is: `text`;
/// * the array of `col <op> ANY/ALL($N)`: the array of the column's declared type;
/// * the count of a `LIMIT`, `OFFSET` or `FETCH`: `bigint`;
/// * the value a reduced `INSERT` or `UPDATE` stores in a column: that column's declared type;
/// * an item of a select list, bare ([`SelectItems`]): `text`, which is what Postgres makes of an
///   output column it has no type for; or, in a set operation of two branches, the type of the
///   other branch's item, which it is resolved against.
///
/// `$N IS [NOT] NULL` types nothing and agrees with the rest. Any other use (a function's
/// argument, an operand of arithmetic or of another operator, a branch of a `CASE` or an argument
/// of `COALESCE`) states none. Postgres does type the parameter there, by rules this does not
/// model, so nothing is assumed, and a cast of that parameter is refused.
fn stated_types(queries: &[Query], inf: &mut Inferred, ctx: &Context) -> HashMap<u32, Uses> {
    use sqlparser::ast::UnaryOperator;
    // The address of a placeholder -> what the expression around it says.
    let mut stated: HashMap<usize, Use> = HashMap::new();
    let (cols, uf) = (&inf.col, &mut inf.uf);
    // The type an operand a parameter meets is read as.
    let mut partner = |e: &Expr| -> Option<Use> {
        let e = unwrap_nested(e);
        match e {
            Expr::Cast { data_type, .. } => return Some(Use::Stated(type_key(&data_type.to_string()))),
            Expr::TypedString(t) => return Some(Use::Stated(type_key(&t.data_type.to_string()))),
            // `date(x)` is a cast to `date` written as a call, whatever `x` is. No other call is
            // read: most take their result's type from their arguments (`lower` of a range).
            Expr::Function(f) if is_date_call(f) => return Some(Use::Stated(TypeKey::Scalar(Ty::Date))),
            Expr::Value(v) if plain_string(&v.value) => return Some(Use::Stated(TypeKey::Scalar(Ty::Str))),
            Expr::Value(v) => return literal_key(&v.value).map(Use::Stated),
            Expr::UnaryOp { op: UnaryOperator::Minus | UnaryOperator::Plus, expr } => {
                return match unwrap_nested(expr) {
                    Expr::Value(v) if matches!(v.value, SqlValue::Number(..)) => literal_key(&v.value).map(Use::Stated),
                    _ => None,
                }
            }
            Expr::Identifier(_) | Expr::CompoundIdentifier(_) => {}
            _ => return None,
        }
        let Atom::Col(table, col) = cols.get(&nid(e))? else { return None };
        Some(match ctx.declared.and_then(|cat| crate::infer::declared_type(cat, table, col)) {
            Some(declared) => Use::Stated(type_key(declared)),
            // No DDL says, so the column is what inference made of its uses, where it made
            // anything of them.
            None => match uf.get_type(&Atom::Col(table.clone(), col.clone())) {
                Some(ty) if ty != Ty::Opaque => Use::Stated(TypeKey::Scalar(ty)),
                _ => Use::Fits,
            },
        })
    };
    let array = |u: Option<Use>| {
        u.map(|u| match u {
            Use::Stated(k) => Use::Stated(TypeKey::Array(Box::new(k))),
            Use::Fits => Use::Fits,
        })
    };
    // `LIKE` and its kin are operators over `text` where one operand is: Postgres finds no
    // `varchar ~~ varchar` and takes `text ~~ text`. Over any other type nothing is read.
    let text = |u: Option<Use>| match u {
        Some(Use::Stated(TypeKey::Scalar(Ty::Str))) | Some(Use::Fits) => u,
        _ => None,
    };
    // The parameters of each `IN` list, which take one type between them.
    let mut ties: Vec<Vec<u32>> = Vec::new();
    for q in queries {
        let counts = crate::infer::pagination_count_ids(q);
        let _ = sweep(q, |e| {
            use sqlparser::ast::{BinaryOperator as B, InTarget};
            // A count is a `bigint`. Under a cast it is the cast's operand, which the cast types.
            if counts.contains(&nid(e)) {
                note(&mut stated, e, Some(Use::Stated(TypeKey::Scalar(Ty::Int))));
            }
            match e {
                Expr::Cast { expr, data_type, .. } => {
                    note(&mut stated, expr, Some(Use::Stated(type_key(&data_type.to_string()))))
                }
                Expr::BinaryOp { left, op: B::Eq | B::NotEq | B::Lt | B::LtEq | B::Gt | B::GtEq, right } => {
                    compared(&mut stated, &mut ties, &mut partner, left, right);
                }
                Expr::IsDistinctFrom(a, b) | Expr::IsNotDistinctFrom(a, b) => {
                    compared(&mut stated, &mut ties, &mut partner, a, b);
                }
                Expr::IsNull(x) | Expr::IsNotNull(x) => note(&mut stated, x, Some(Use::Fits)),
                Expr::In { expr, target: InTarget::List(list), .. } => {
                    let subject = partner(expr);
                    let (says, params) = in_list(subject, list, &mut partner);
                    for p in &params {
                        note(&mut stated, p, says.clone());
                    }
                    ties.push(params.iter().filter_map(|p| placeholder_index(p)).collect());
                    // The subject, where it is the parameter. Postgres compares it with each column
                    // among the items in turn and resolves it with the rest together, and either
                    // way it is of their type where they have one between them.
                    if placeholder_index(unwrap_nested(expr)).is_some() {
                        note(&mut stated, expr, one_type(list.iter().map(&mut partner)));
                    }
                }
                Expr::Between { expr, low, high, .. } => {
                    note(&mut stated, low, partner(expr));
                    note(&mut stated, high, partner(expr));
                    // `$1 BETWEEN lo AND hi` is `$1 >= lo AND $1 <= hi`: the low bound types it,
                    // and the high bound is asked to agree.
                    if placeholder_index(unwrap_nested(expr)).is_some() {
                        note(&mut stated, expr, one_type([partner(low), partner(high)]));
                    }
                }
                Expr::Like { expr, pattern, any: false, .. }
                | Expr::ILike { expr, pattern, any: false, .. }
                | Expr::SimilarTo { expr, pattern, .. } => {
                    note(&mut stated, pattern, text(partner(expr)));
                    note(&mut stated, expr, text(partner(pattern)));
                }
                Expr::AnyOp { left, right, .. } | Expr::AllOp { left, right, .. } => {
                    note(&mut stated, right, array(partner(left)))
                }
                _ => {}
            }
            Ok(())
        });
    }
    if let Some(stores) = ctx.stores {
        for (q, s) in queries.iter().zip(stores) {
            for (value, declared) in s.stored_values(q) {
                note(&mut stated, value, Some(Use::Stated(type_key(declared))));
            }
        }
    }

    // The select lists. Last, so that a value a reduced statement stores keeps the type of its
    // column.
    for (i, q) in queries.iter().enumerate() {
        let mut items = SelectItems {
            reduced: ctx.stores.is_some_and(|s| s.get(i).is_some_and(|s| s.stores_any())).then_some(q as *const Query),
            operands: HashSet::new(),
            partner: &mut partner,
            stated: &mut stated,
        };
        let ControlFlow::Continue(()) = sqlparser::ast::Visit::visit(q, &mut items);
    }

    let mut uses: HashMap<u32, Uses> = HashMap::new();
    for q in queries {
        let _ = sweep(q, |e| {
            let Some(n) = placeholder_index(e) else { return Ok(()) };
            let u = uses.entry(n).or_default();
            match stated.get(&nid(e)) {
                Some(Use::Stated(k)) => {
                    if !u.keys.contains(k) {
                        u.keys.push(k.clone());
                    }
                }
                Some(Use::Fits) => {}
                None => u.silent = true,
            }
            Ok(())
        });
    }
    pool(&ties, &mut uses, |a, b| {
        let mut both = a.clone();
        both.keys.extend(b.keys.iter().filter(|k| !a.keys.contains(k)).cloned());
        both.silent |= b.silent;
        both
    });
    uses
}

/// Record what the expression around `param` says of it, where `param` is a bare parameter and
/// nothing closer to it has said anything yet.
fn note(stated: &mut HashMap<usize, Use>, param: &Expr, says: Option<Use>) {
    let p = unwrap_nested(param);
    if let (Some(_), Some(u)) = (placeholder_index(p), says) {
        stated.entry(nid(p)).or_insert(u);
    }
}

/// What the two operands of a comparison say of a bare parameter among them.
///
/// * **A parameter beside an operand that is read** takes its type (`partner`).
/// * **Two bare parameters** are one type between them, so they are tied (`ties`, for [`pool`]):
///   Postgres compares two operands of unknown type as `text`, and where another use typed one of
///   them first, the other takes that type. In `a = $1 AND $1 = $2 AND s = $2::text` over an
///   integer `a`, `$2` is an integer, and the cast converts it. So each states `text`, and a
///   cast of either is dropped only where no use of either says otherwise.
/// * **Two rows** of as many fields are compared field by field: `(s, a) = ($1, $2)` types `$1` by
///   `s` and `$2` by `a`.
fn compared(
    stated: &mut HashMap<usize, Use>,
    ties: &mut Vec<Vec<u32>>,
    partner: &mut dyn FnMut(&Expr) -> Option<Use>,
    left: &Expr,
    right: &Expr,
) {
    let (l, r) = (unwrap_nested(left), unwrap_nested(right));
    match (l, r) {
        (Expr::Tuple(ls), Expr::Tuple(rs)) if ls.len() == rs.len() => {
            for (l, r) in ls.iter().zip(rs) {
                compared(stated, ties, partner, l, r);
            }
        }
        _ => match (placeholder_index(l), placeholder_index(r)) {
            (Some(a), Some(b)) => {
                let text = Use::Stated(TypeKey::Scalar(Ty::Str));
                note(stated, l, Some(text.clone()));
                note(stated, r, Some(text));
                ties.push(vec![a, b]);
            }
            _ => {
                note(stated, l, partner(r));
                note(stated, r, partner(l));
            }
        },
    }
}

/// The one type a parameter is read as beside `others`, where each of them is read and they
/// state one type between them. A column of unknown type fits whatever the rest state.
fn one_type(others: impl IntoIterator<Item = Option<Use>>) -> Option<Use> {
    let mut one = None;
    for other in others {
        one = match (one, other?) {
            (None, u) | (Some(Use::Fits), u) => Some(u),
            (Some(have), Use::Fits) => Some(have),
            (Some(Use::Stated(have)), Use::Stated(k)) if have == k => Some(Use::Stated(have)),
            _ => return None,
        };
    }
    one
}

/// Whether a call is `date(x)`: Postgres's `date` over one argument, which is a `date` for a
/// timestamp, a `timestamptz`, a `date` or a string alike.
fn is_date_call(f: &Function) -> bool {
    let name = obj_name(&f.name).to_lowercase();
    let one_plain_argument = match &f.args {
        FunctionArguments::List(l) => {
            l.duplicate_treatment.is_none()
                && l.clauses.is_empty()
                && matches!(l.args.as_slice(), [FunctionArg::Unnamed(FunctionArgExpr::Expr(_))])
        }
        _ => false,
    };
    matches!(name.as_str(), "date" | "pg_catalog.date")
        && one_plain_argument
        && f.over.is_none()
        && f.filter.is_none()
        && f.within_group.is_empty()
}

/// Whether a constant is a plain string, which has no type until its context gives it one.
/// `B'..'`, `X'..'` and `N'..'` have one.
fn plain_string(v: &SqlValue) -> bool {
    matches!(
        v,
        SqlValue::SingleQuotedString(_)
            | SqlValue::DollarQuotedString(_)
            | SqlValue::EscapedStringLiteral(_)
            | SqlValue::UnicodeStringLiteral(_)
    )
}

/// What the select lists of a query, at any depth, say of the bare parameters among their items.
///
/// Postgres resolves an output column of unknown type to `text` once it has read the query, in a
/// statement's own list and in a subquery's, a derived table's and a CTE's alike: `a IN (SELECT $1)`
/// over an integer `a` compares an integer with a `text`, and does not prepare. Two lists are read
/// otherwise:
///
/// * **The branches of a set operation** are resolved against each other first, column by column,
///   so a bare parameter takes the type of the other branch's item, and `text` where that has none
///   either. Read for two branches that are plain `SELECT`s of as many items. A longer chain is
///   resolved pair by pair from the left, and is not read.
/// * **The query a reduced `INSERT` or `UPDATE` became** stores its items in columns, which is
///   what types them (`Stores::stored_values`). Its own list is not read here.
struct SelectItems<'a> {
    /// The query a reduced `INSERT` or `UPDATE` became, where this is one.
    reduced: Option<*const Query>,
    /// The parenthesised queries that are an operand of a set operation.
    operands: HashSet<*const Query>,
    partner: &'a mut dyn FnMut(&Expr) -> Option<Use>,
    stated: &'a mut HashMap<usize, Use>,
}

impl SelectItems<'_> {
    /// The expressions of a select list by position, or nothing where it has a wildcard, which is
    /// as many positions as a table has columns.
    fn items(select: &sqlparser::ast::Select) -> Option<Vec<&Expr>> {
        use sqlparser::ast::SelectItem;
        select
            .projection
            .iter()
            .map(|item| match item {
                SelectItem::UnnamedExpr(e) | SelectItem::ExprWithAlias { expr: e, .. } => Some(e),
                _ => None,
            })
            .collect()
    }

    fn operands(&mut self, body: &SetExpr) {
        match body {
            SetExpr::Query(q) => {
                self.operands.insert(&**q as *const Query);
            }
            SetExpr::SetOperation { left, right, .. } => {
                self.operands(left);
                self.operands(right);
            }
            _ => {}
        }
    }

    /// The type the item of the other branch gives a bare parameter.
    fn across(&mut self, other: &Expr) -> Option<Use> {
        match unwrap_nested(other) {
            Expr::Value(v) if matches!(v.value, SqlValue::Null | SqlValue::Placeholder(_)) => {
                Some(Use::Stated(TypeKey::Scalar(Ty::Str)))
            }
            other => (self.partner)(other),
        }
    }
}

impl Visitor for SelectItems<'_> {
    type Break = std::convert::Infallible;

    fn pre_visit_query(&mut self, q: &Query) -> ControlFlow<Self::Break> {
        let this = q as *const Query;
        if self.reduced == Some(this) {
            return ControlFlow::Continue(());
        }
        match &*q.body {
            SetExpr::Select(select) if !self.operands.contains(&this) => {
                use sqlparser::ast::SelectItem;
                for item in &select.projection {
                    if let SelectItem::UnnamedExpr(e) | SelectItem::ExprWithAlias { expr: e, .. } = item {
                        note(self.stated, e, Some(Use::Stated(TypeKey::Scalar(Ty::Str))));
                    }
                }
            }
            SetExpr::SetOperation { left, right, .. } => {
                self.operands(left);
                self.operands(right);
                if let (SetExpr::Select(l), SetExpr::Select(r)) = (&**left, &**right) {
                    if let (Some(l), Some(r)) = (Self::items(l), Self::items(r)) {
                        if l.len() == r.len() {
                            for (a, b) in l.into_iter().zip(r) {
                                let (for_a, for_b) = (self.across(b), self.across(a));
                                note(self.stated, a, for_a);
                                note(self.stated, b, for_b);
                            }
                        }
                    }
                }
            }
            _ => {}
        }
        ControlFlow::Continue(())
    }
}

/// What an `IN` list says of the bare parameters among its items, and which items they are.
///
/// Postgres resolves one common type over the subject and the items that are not columns of the
/// query, where there are two or more such items, and a bare parameter among them takes it. An
/// item with no type of its own (a string literal, `NULL`, a parameter nothing has typed yet) adds
/// nothing to that type. So the list's parameters have the subject's type where every other item
/// is one of those, a cast or a literal of that type, or a column of it. Any other item may widen
/// the common type: `r IN ($1, $2::float8)` over a `real` `r` makes `$1` a `double precision`. Then
/// nothing is said of the parameters.
///
/// A sibling parameter that another use typed first widens it as well: in `f = $2 AND r IN ($1, $2)`
/// over a `float8` `f`, `$2` is a `double precision` by the time the list is read, and makes `$1`
/// one. So the list's parameters are returned together, for [`pool`] to hold to one type.
fn in_list<'e>(
    subject: Option<Use>,
    items: &'e [Expr],
    column: &mut dyn FnMut(&Expr) -> Option<Use>,
) -> (Option<Use>, Vec<&'e Expr>) {
    use sqlparser::ast::UnaryOperator;
    let params: Vec<&Expr> =
        items.iter().map(unwrap_nested).filter(|i| placeholder_index(i).is_some()).collect();
    let mut says = subject;
    for item in items.iter().map(unwrap_nested) {
        // The type an item brings to the common type, where it brings one.
        let typed = match item {
            Expr::Value(v) => match &v.value {
                SqlValue::Placeholder(_) | SqlValue::Null => continue,
                // A plain string constant has no type until its context gives it one. `B'..'`,
                // `X'..'` and `N'..'` have one, which is not read.
                SqlValue::SingleQuotedString(_)
                | SqlValue::DollarQuotedString(_)
                | SqlValue::EscapedStringLiteral(_)
                | SqlValue::UnicodeStringLiteral(_) => continue,
                other => literal_key(other),
            },
            Expr::UnaryOp { op: UnaryOperator::Minus | UnaryOperator::Plus, expr } => match unwrap_nested(expr) {
                Expr::Value(v) if matches!(v.value, SqlValue::Number(..)) => literal_key(&v.value),
                _ => None,
            },
            Expr::Cast { data_type, .. } => Some(type_key(&data_type.to_string())),
            Expr::Identifier(_) | Expr::CompoundIdentifier(_) => match column(item) {
                // A column of unknown type fits the list, whatever the list's type is.
                Some(Use::Fits) => continue,
                Some(Use::Stated(k)) => Some(k),
                None => None,
            },
            _ => None,
        };
        says = match (says, typed) {
            (Some(Use::Stated(have)), Some(k)) if have == k => Some(Use::Stated(have)),
            // The subject is a column of unknown type: the list is of the type its items state.
            (Some(Use::Fits), Some(k)) => Some(Use::Stated(k)),
            _ => None,
        };
        if says.is_none() {
            break;
        }
    }
    (says, params)
}

/// The key of the type Postgres gives a number or a boolean constant, where that is one of the
/// classes: an integer, a `numeric` (a decimal point, an exponent, or digits beyond `bigint`), a
/// boolean. `None` for any other constant.
fn literal_key(v: &SqlValue) -> Option<TypeKey> {
    match v {
        SqlValue::Number(n, _) => {
            Some(TypeKey::Scalar(if n.parse::<i64>().is_ok() { Ty::Int } else { Ty::Real }))
        }
        SqlValue::Boolean(_) => Some(TypeKey::Scalar(Ty::Bool)),
        _ => None,
    }
}

/// Hold the parameters of each group in `ties` to one answer: `join` of what was found for each.
/// The groups are closed under sharing a parameter first, so `a IN ($1, $2) AND b IN ($2, $3)`
/// is one group of three.
fn pool<T: Clone>(ties: &[Vec<u32>], found: &mut HashMap<u32, T>, join: impl Fn(&T, &T) -> T) {
    let mut groups: Vec<Vec<u32>> = Vec::new();
    for tie in ties.iter().filter(|t| t.len() > 1) {
        let (mut joined, rest): (Vec<Vec<u32>>, Vec<Vec<u32>>) =
            groups.into_iter().partition(|g| g.iter().any(|n| tie.contains(n)));
        joined.push(tie.clone());
        groups = rest;
        groups.push(joined.concat());
    }
    for group in groups {
        let mut all: Option<T> = None;
        for n in &group {
            if let Some(t) = found.get(n) {
                all = Some(match &all {
                    Some(have) => join(have, t),
                    None => t.clone(),
                });
            }
        }
        if let Some(all) = all {
            for n in group {
                found.insert(n, all.clone());
            }
        }
    }
}

/// What to do with one cast. Recorded against the cast node's [`nid`] by [`decide`].
#[derive(Clone, Debug)]
enum Decision {
    /// Rules 1 and 4: the cast goes and its operand takes its place. Carries the operand's address
    /// before the move, so the identity maps can be re-keyed after it.
    Hoist { operand: usize },
    /// Rules 2/3 and 5a: a real cast survives, retargeted to the prover's spelling of the type.
    Retarget(Ty),
    /// Rule 5b: wrap the operand in a shared symbol. Carries the index into [`Rewrite::qcasts`] and
    /// every address inside the operand, which the preprocessor's `.copy()` invalidates.
    Wrap { idx: usize, copied: Vec<usize> },
}

/// The prover's spelling of a type, as a `DataType` [`crate::types::map_type`] will read back.
fn data_type(t: Ty) -> DataType {
    match t {
        Ty::Int => DataType::Integer(None),
        // `numeric`, not `double precision`: a float is opaque to `map_type`, and `Ty::Real` is exact.
        Ty::Real => DataType::Numeric(ExactNumberInfo::None),
        Ty::Str => DataType::Varchar(None),
        Ty::Bool => DataType::Boolean,
        Ty::Date => DataType::Date,
        Ty::Time => DataType::Time(None, TimezoneInfo::None),
        Ty::Timestamp => DataType::Timestamp(None, TimezoneInfo::None),
        Ty::TimestampTz => DataType::Timestamp(None, TimezoneInfo::WithTimeZone),
        Ty::Interval => DataType::Interval { fields: None, precision: None },
        Ty::Opaque => DataType::Varbinary(None),
    }
}

/// Whether `t` is one of the temporal types, which never cross into each other or into anything else
/// without a named conversion (see `types.rs`).
fn is_temporal(t: Ty) -> bool {
    matches!(t, Ty::Date | Ty::Time | Ty::Timestamp | Ty::TimestampTz | Ty::Interval)
}

/// `name(arg)` — the shape both `qcastK` and `qpN` take.
fn call(name: &str, arg: Expr) -> Expr {
    Expr::Function(Function {
        name: ObjectName(vec![ObjectNamePart::Identifier(Ident::new(name))]),
        uses_odbc_syntax: false,
        parameters: FunctionArguments::None,
        args: FunctionArguments::List(FunctionArgumentList {
            duplicate_treatment: None,
            args: vec![FunctionArg::Unnamed(FunctionArgExpr::Expr(arg))],
            clauses: Vec::new(),
        }),
        filter: None,
        null_treatment: None,
        over: None,
        within_group: Vec::new(),
    })
}

/// `$N` written as a placeholder, which is what rule 1 keys on.
///
/// Narrower than [`crate::infer::param_index`], which also accepts the substituted `qpN(0)` form:
/// the preprocessor's rule 1 tests for `exp.Parameter` specifically, and a `qpN(0)` reaching here
/// would be a call, taking the rule-4/5 path. Keeping the distinction keeps the port honest on
/// already-prepared input.
fn placeholder_index(e: &Expr) -> Option<u32> {
    match e {
        Expr::Value(v) => match &v.value {
            SqlValue::Placeholder(p) => p.strip_prefix('$')?.parse().ok(),
            _ => None,
        },
        _ => None,
    }
}

/// Strip the parentheses off `(expr)::T` the way the preprocessor does before it classifies the
/// operand — so `($1)::int` is rule 1, not "cast over unsupported operand".
pub(crate) fn unwrap_nested(mut e: &Expr) -> &Expr {
    while let Expr::Nested(inner) = e {
        e = inner;
    }
    e
}

/// Every address in a subtree, for the copy semantics of rule 5b.
fn subtree_ids(e: &Expr) -> Vec<usize> {
    let mut out = vec![nid(e)];
    let _ = sqlparser::ast::visit_expressions(e, |x| {
        out.push(nid(x));
        ControlFlow::<()>::Continue(())
    });
    out
}

/// A literal or `NULL` — the operands of rules 2/3.
///
/// `Expr::Value` also covers placeholders, which rule 1 has already claimed by the time this runs.
fn is_literal(e: &Expr) -> bool {
    matches!(e, Expr::Value(v) if !matches!(v.value, SqlValue::Placeholder(_)))
}

/// The dedup key's view of a cast target: two casts share a `qcast` symbol exactly when this agrees.
///
/// Sharing a symbol is what lets an untouched conversion cancel between the two sides of a pair, and
/// it is also the one way this rewrite could prove something false -- two casts that compute
/// different things under one name. So the rule is: collapse a spelling only where the spelling is
/// all that differs.
///
/// * An unqualified mappable target collapses to its [`Ty`], so `x::text` and `x::varchar` keep
///   sharing one symbol and a pair that merely respells a cast still cancels.
/// * A qualified one keeps its own spelling. `varchar(8)` and `varchar(4)` are different truncations
///   of the same column, and the [`Ty`] alone cannot tell them apart.
/// * An unmappable one keeps its own spelling too -- it has no [`Ty`] to collapse to, and every
///   unmappable target shares [`Ty::Opaque`], so collapsing would make `x::jsonb` and `x::uuid[]` one
///   function. Z3 identifies an uninterpreted function by name and sorts, and both of those carry as
///   VARBINARY, so the name is the only thing keeping them apart.
fn canon_target(txt: &str, target: Option<Ty>, qualified: bool) -> String {
    match target {
        Some(t) if !qualified => t.sql().to_string(),
        _ => canon_type_name(txt),
    }
}

/// The dedup key's view of a cast operand: its text, with unquoted names folded to lower case.
///
/// The text stands in for what the operand computes, its type included, and Postgres folds an
/// unquoted name before it reads it, so `SUM(x)` and `sum(X)` are one call and their casts one
/// function. Literals and quoted names are compared as written.
fn operand_key(op: &Expr) -> String {
    let mut e = op.clone();
    let _ = e.visit(&mut FoldNames);
    e.to_string()
}

struct FoldNames;

impl VisitorMut for FoldNames {
    type Break = ();

    fn pre_visit_expr(&mut self, e: &mut Expr) -> ControlFlow<Self::Break> {
        let fold = |id: &mut Ident| {
            if id.quote_style.is_none() {
                id.value = id.value.to_lowercase();
            }
        };
        match e {
            Expr::Identifier(id) => fold(id),
            Expr::CompoundIdentifier(ids) => ids.iter_mut().for_each(fold),
            Expr::Function(f) => f.name.0.iter_mut().for_each(|p| {
                if let ObjectNamePart::Identifier(id) = p {
                    fold(id)
                }
            }),
            _ => {}
        }
        ControlFlow::Continue(())
    }
}

/// The parameters every occurrence of which, in either query, is the operand of an unqualified cast:
/// `$6::int` wherever `$6` appears. Postgres types such a parameter by its first use, which is then a
/// cast, so its type is the cast's whatever order it reads the statement in. Read before rule 1 drops
/// the casts.
pub fn cast_everywhere(queries: &[Query]) -> std::collections::BTreeSet<u32> {
    let (mut seen, mut cast) = (HashMap::<u32, usize>::new(), HashMap::<u32, usize>::new());
    for q in queries {
        let _ = sweep(q, |e| {
            match e {
                Expr::Value(_) => {
                    if let Some(n) = placeholder_index(e) {
                        *seen.entry(n).or_default() += 1;
                    }
                }
                // `has_typmod` and not [`map_type_name`]'s flag, which is never set for an array:
                // `$6::varchar(1)[]` truncates, and is not the parameter's type and nothing more.
                Expr::Cast { expr, data_type, .. } if !has_typmod(&data_type.to_string()) => {
                    if let Some(n) = placeholder_index(unwrap_nested(expr)) {
                        *cast.entry(n).or_default() += 1;
                    }
                }
                _ => {}
            }
            Ok(())
        });
    }
    seen.into_iter().filter(|(n, k)| cast.get(n) == Some(k)).map(|(n, _)| n).collect()
}

/// A type as a message names it.
fn ty_name(t: Ty) -> &'static str {
    match t {
        Ty::Int => "integer",
        Ty::Real => "numeric",
        Ty::Str => "text",
        Ty::Bool => "boolean",
        Ty::Date => "date",
        Ty::Time => "time",
        Ty::Timestamp => "timestamp",
        Ty::TimestampTz => "timestamptz",
        Ty::Interval => "interval",
        Ty::Opaque => "an opaque type",
    }
}

/// Read the untouched tree and record what should happen to each cast.
fn decide(
    queries: &[Query],
    inf: &mut Inferred,
    ctx: &Context,
    dec: &mut HashMap<usize, Decision>,
    rw: &mut Rewrite,
) -> Result<()> {
    let uses = stated_types(queries, inf, ctx);
    // (canonical target, operand text) -> index into `rw.qcasts`. This key decides which casts are
    // *the same function*, so everything the target says that changes what the cast computes has to
    // be in it -- see `canon_target`.
    let mut seen: HashMap<(String, String), usize> = HashMap::new();
    for q in queries {
        sweep(q, |e| {
            let Expr::Cast { expr, data_type: dt, .. } = e else { return Ok(()) };
            let txt = dt.to_string();
            // A `citext` or `char(n)` value is refused wherever it appears (`types::UNFAITHFUL`). A
            // `qcast` would carry it as an opaque value, whose `=` is the prover's equality.
            if let Some(u) = crate::types::unfaithful_type(&txt) {
                return Err(crate::types::unfaithful_refusal(u));
            }
            let (target, qualified) = map_type_name(&txt);
            // A target with no `Ty` is still a deterministic function of its operand; what is missing
            // is an interpretation, not a value. `Ty::Opaque` carries it -- VARBINARY, which the
            // prover gives an equality-only sort -- and rule 5b below wraps the cast in a symbol
            // named after the target. See [`map_type_name`] for why inference does not do the same.
            let tq = target.unwrap_or(Ty::Opaque);
            let op = unwrap_nested(expr);

            // A typmod is a computation, not a type: `$1::varchar(2)` truncates and
            // `$1::timestamp(0)` rounds, so only an unqualified cast over a parameter is the
            // parameter's type and nothing more. A qualified one takes rule 5b, keyed on the
            // qualified spelling.
            //
            // And only a cast to the type inference gave the parameter. Postgres types `$N` at its
            // first use, which inference does not follow, so a cast to another type may be a
            // conversion it runs: in `n = $1 AND $1::int = 0` over a `numeric` `n`, `$1` is a
            // `numeric` and `$1::int` rounds it, and dropping the cast made the query one plan with
            // `n = $1 AND $1 = 0`.
            //
            // Inference's test is not enough, since it reads no type off many uses and keeps some
            // types apart that it should not. So whatever the target, every use of the parameter, in
            // both queries, has to be one that states the cast's type: see the module docs. A use
            // that states another type makes the parameter that type in its statement, and this
            // cast a conversion. A use that states none leaves the parameter's type to an order of
            // reading the frontend does not follow. One with a typmod, on itself (`$1::bit(3)`) or
            // on its element (`$1::varchar(1)[]`), computes something even where every use agrees
            // on the type, and takes rule 5b like any other qualified cast.
            let key = type_key(&txt);
            let typmod = has_typmod(&txt);
            if let Some(n) = placeholder_index(op) {
                if !uses.get(&n).is_some_and(|u| u.all_state(&key)) && rw.mistyped.is_none() {
                    rw.mistyped = Some(unsupported(format!(
                        "${n} is cast to {txt}, but not every use of ${n} states that type: Postgres \
                         types an untyped parameter at its first use, and a cast to another type \
                         converts it"
                    )));
                }
            }
            if let Some(n) = placeholder_index(op).filter(|_| !typmod) {
                let own = inf.params.get(&n).copied().unwrap_or(Ty::Opaque);
                // Inference types an array parameter by its element, so an array cast states the
                // element's type. A parameter inference has no evidence for at all is left to the
                // cast: for a target it has a type for, nothing says the parameter's is another, and
                // for any other target every use was asked above.
                let stated = match crate::normalize::array_elem_type(dt) {
                    Some(elem) => map_type_name(&elem.to_string()).0.unwrap_or(Ty::Opaque),
                    None => tq,
                };
                if own != Ty::Opaque && own != stated {
                    return Err(unsupported(format!(
                        "${n} is cast to {txt} but inferred as {}: Postgres types an untyped \
                         parameter at its first use, and a cast to another type converts it",
                        ty_name(own)
                    )));
                }
                dec.insert(nid(e), Decision::Hoist { operand: nid(op) });
                return Ok(());
            }
            // The same holds over a literal: `'abc'::varchar(2)` is `'ab'`, and retargeting it to the
            // bare type would make it a cast between equal types, which is the identity.
            if is_literal(op) && !qualified {
                dec.insert(nid(e), Decision::Retarget(tq));
                return Ok(());
            }

            // Only rules 4 and 5a consult the operand's type; rule 5b does not, and its symbol is
            // declared from the target alone — `declarations` writes `decl(q.ret, ..)` — so an
            // operand nobody can type is not a reason to refuse. It is a reason to take 5b, and
            // `Ty::Opaque` gets it there on its own:
            //
            // * rule 4 is gated on `target.is_some() && cqt == tq`, and [`map_type_name`] returns
            //   `Some` only for Int/Real/Str/Bool — never `Some(Ty::Opaque)` — so the two cannot
            //   both hold;
            // * rule 5a needs `cqt == Ty::Int`.
            //
            // Both gates are therefore unreachable, and 5b is the conservative branch: it neither
            // drops the cast nor retargets it, it names it. Same shape as the argument for an
            // unmappable *target* above.
            let cqt = match op {
                Expr::Identifier(_) | Expr::CompoundIdentifier(_) => {
                    match inf.col.get(&nid(op)).cloned() {
                        Some(a) => inf.uf.get_type(&a).unwrap_or(Ty::Opaque),
                        None => Ty::Opaque,
                    }
                }
                // A cast states its own result type, which makes a nested cast the *most* precise
                // operand there is — better evidence than inference, and read from the untouched
                // tree, so `decide` stays order-independent. `kind` is ignored here as it is for the
                // outer cast: `TRY_CAST(x AS int)` still has type `int`, it just yields NULL instead
                // of erroring.
                Expr::Cast { data_type, .. } => {
                    map_type_name(&data_type.to_string()).0.unwrap_or(Ty::Opaque)
                }
                // The old `if is_opaque_call(op)` guard excluded exactly the *modelled* functions —
                // the ones whose return type `fn_ret` knows — and bought nothing: both sides of the
                // guard ended in the same `unwrap_or(Ty::Opaque)`.
                Expr::Function(f) => builtin_rtype(&obj_name(&f.name))
                    .or_else(|| inf.fn_ret.get(&nid(op)).copied())
                    .unwrap_or(Ty::Opaque),
                // The two kinds still worth refusing. `lower.rs`'s `lower_expr` has no arm for a
                // tuple, so wrapping one in a `qcast` would move the refusal rather than lift it;
                // and a `qcast` around an array literal would cost the exact OR-expansion
                // `lower_quantified` gives one under `= ANY`.
                other @ (Expr::Array(_) | Expr::Tuple(_)) => {
                    return Err(unsupported(format!(
                        "cast over unsupported operand {}",
                        operand_kind(other)
                    )))
                }
                _ => Ty::Opaque,
            };

            // `target.is_some()` gates the identity rule, and it must: with an unmappable target
            // both sides of `cqt == tq` can be `Ty::Opaque` -- an unknown column cast to `jsonb` --
            // and dropping the cast then equates `x::jsonb` with a bare `x`. Not knowing what a cast
            // computes is the opposite of knowing it computes nothing.
            if target.is_some() && !qualified && cqt == tq {
                dec.insert(nid(e), Decision::Hoist { operand: nid(op) });
            } else if cqt == Ty::Int && tq == Ty::Real {
                dec.insert(nid(e), Decision::Retarget(Ty::Real));
            } else if target.is_some()
                && !qualified
                && cqt != Ty::Opaque
                && (is_temporal(cqt) || is_temporal(tq))
            {
                // A crossing between two known types, one of them temporal: the cast stays, and the
                // lowering turns it into the conversion named after both types
                // (`types::lower_cast`). Named after the types rather than after this operand's text,
                // because a conversion is a function of its source type, its target and its value,
                // and the source type is known here -- so `ts::date` on both sides of a pair is one
                // function, where a `qcast` keyed on each operand's text would split it. An operand
                // of unknown type still takes rule 5b below.
                dec.insert(nid(e), Decision::Retarget(tq));
            } else {
                // Keyed on the operand's *pre-rewrite* text, so `(x::varchar)::varchar(8)` and
                // `x::varchar(8)` get separate symbols even though the inner cast is about to be
                // hoisted away and the two render identically afterwards. Splitting is sound (see
                // the module docs); it costs a cancellation, which is the same trade the key
                // already makes everywhere else.
                let key = (canon_target(&txt, target, qualified), operand_key(op));
                let idx = *seen.entry(key).or_insert_with(|| {
                    rw.qcasts.push(QCast {
                        name: format!("qcast{}", rw.qcasts.len()),
                        ret: tq,
                    });
                    rw.qcasts.len() - 1
                });
                dec.insert(nid(e), Decision::Wrap { idx, copied: subtree_ids(op) });
            }
            Ok(())
        })?;
    }
    Ok(())
}

/// A name for the operand shape in the refusal message. The preprocessor prints sqlglot's node
/// class; this prints the nearest sqlparser equivalent, since the two taxonomies do not line up.
fn operand_kind(e: &Expr) -> &'static str {
    match e {
        Expr::Cast { .. } => "Cast",
        Expr::Function(_) => "Func",
        Expr::BinaryOp { .. } | Expr::UnaryOp { .. } => "Arithmetic",
        Expr::Case { .. } => "Case",
        Expr::Subquery(_) => "Subquery",
        Expr::Tuple(_) => "Tuple",
        Expr::Array(_) => "Array",
        _ => "Expr",
    }
}

/// Apply the decisions bottom-up. See the module docs for why the keys survive.
struct Apply<'a> {
    dec: &'a HashMap<usize, Decision>,
    qcasts: &'a [QCast],
    /// `(old address, new address)` for each hoisted operand.
    remap: Vec<(usize, usize)>,
    /// Addresses the preprocessor's `.copy()` would have invalidated.
    dead: Vec<usize>,
}

impl VisitorMut for Apply<'_> {
    type Break = FrontendError;

    fn post_visit_expr(&mut self, e: &mut Expr) -> ControlFlow<Self::Break> {
        let Some(d) = self.dec.get(&nid(e)).cloned() else { return ControlFlow::Continue(()) };
        // Only cast nodes were given decisions, and nothing has overwritten this one: its
        // descendants were rewritten in place, which does not disturb the node itself.
        if !matches!(e, Expr::Cast { .. }) {
            return ControlFlow::Continue(());
        }
        if let Decision::Retarget(t) = d {
            if let Expr::Cast { data_type: dt, .. } = e {
                *dt = data_type(t);
            }
            return ControlFlow::Continue(());
        }
        let here = nid(e);
        let Expr::Cast { expr, .. } = std::mem::replace(e, Expr::Identifier(Ident::new(""))) else {
            unreachable!("checked just above")
        };
        let mut op = *expr;
        while let Expr::Nested(inner) = op {
            op = *inner;
        }
        match d {
            Decision::Hoist { operand } => {
                *e = op;
                self.remap.push((operand, here));
            }
            Decision::Wrap { idx, copied } => {
                *e = call(&self.qcasts[idx].name, op);
                self.dead.extend(copied);
            }
            Decision::Retarget(_) => unreachable!("handled above"),
        }
        ControlFlow::Continue(())
    }
}

/// Refuse every cast that is neither `CAST(x AS T)` nor `x::T`: `TRY_CAST(x AS T)` and
/// `SAFE_CAST(x AS T)`, which yield `NULL` where `CAST` raises an error, and are not Postgres syntax.
///
/// sqlparser accepts both under the Postgres dialect and builds them the node it builds for a
/// `CAST`, told apart only by `kind`. Nothing past this point reads `kind` — not the rules above, not
/// inference, not lowering — so each would be lowered as a plain `CAST`, and a pair that differs
/// only in which of the two it wrote would lower to one plan. Run on the parsed input before any
/// rewrite, so no rule gets to hoist or wrap one first. A kind sqlparser adds later is refused too.
pub fn refuse_foreign_kinds(statements: &[Statement]) -> Result<()> {
    for st in statements {
        let found = visit_expressions(st, |e| match e {
            Expr::Cast { kind: CastKind::Cast | CastKind::DoubleColon, .. } => ControlFlow::Continue(()),
            Expr::Cast { kind, .. } => ControlFlow::Break(kind.clone()),
            _ => ControlFlow::Continue(()),
        });
        match found {
            ControlFlow::Continue(()) => {}
            ControlFlow::Break(CastKind::TryCast) => return Err(unsupported("TRY_CAST")),
            ControlFlow::Break(CastKind::SafeCast) => return Err(unsupported("SAFE_CAST")),
            ControlFlow::Break(other) => return Err(unsupported(format!("cast written as {other:?}"))),
        }
    }
    Ok(())
}

/// Rewrite every cast in `queries`, updating `inf`'s identity maps to match.
pub fn rewrite_casts(queries: &mut [Query], inf: &mut Inferred, ctx: &Context) -> Result<Rewrite> {
    let mut dec = HashMap::new();
    let mut rw = Rewrite::default();
    decide(queries, inf, ctx, &mut dec, &mut rw)?;
    if dec.is_empty() {
        return Ok(rw);
    }
    let mut ap = Apply { dec: &dec, qcasts: &rw.qcasts, remap: Vec::new(), dead: Vec::new() };
    for q in queries.iter_mut() {
        if let ControlFlow::Break(err) = q.visit(&mut ap) {
            return Err(err);
        }
    }
    let (remap, dead) = (ap.remap, ap.dead);
    for (old, new) in remap {
        if let Some(a) = inf.col.remove(&old) {
            inf.col.insert(new, a);
        }
        if let Some(t) = inf.fn_ret.remove(&old) {
            inf.fn_ret.insert(new, t);
        }
    }
    let dead: HashSet<usize> = dead.into_iter().collect();
    inf.col.retain(|k, _| !dead.contains(k));
    inf.fn_ret.retain(|k, _| !dead.contains(k));
    Ok(rw)
}

/// Replace every `$N` with `qpN(0)`, the shared uninterpreted constant the prover reads.
///
/// A parameter has one value per execution, the same value everywhere it appears and the same on
/// both sides of the pair. A nullary uninterpreted symbol says exactly that; `0` is a filler
/// argument because the prover's declaration DSL has no nullary form.
pub fn substitute_params(queries: &mut [Query], inf: &mut Inferred) -> Result<()> {
    struct Sub {
        remap: Vec<(usize, usize)>,
    }
    impl VisitorMut for Sub {
        type Break = FrontendError;
        fn post_visit_expr(&mut self, e: &mut Expr) -> ControlFlow<Self::Break> {
            let Some(n) = placeholder_index(e) else { return ControlFlow::Continue(()) };
            let here = nid(e);
            *e = call(&format!("qp{n}"), Expr::Value(SqlValue::Number("0".into(), false).into()));
            self.remap.push((here, here));
            ControlFlow::Continue(())
        }
    }
    let mut sub = Sub { remap: Vec::new() };
    for q in queries.iter_mut() {
        if let ControlFlow::Break(err) = q.visit(&mut sub) {
            return Err(err);
        }
    }
    // A placeholder is a leaf and carries no attribution of its own, so nothing needs re-keying —
    // but the node that replaced it is a *call*, and leaving a stale `fn_ret` entry at that address
    // would hand `qpN` an invented return type during signature synthesis.
    for (_, at) in sub.remap {
        inf.fn_ret.remove(&at);
        inf.col.remove(&at);
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Declaration synthesis
// ---------------------------------------------------------------------------

/// The pieces of [`Inferred`] a signature needs, split out so a walk can borrow the tree while the
/// typing borrows the evidence.
struct Types<'a> {
    col: &'a HashMap<usize, Atom>,
    fn_ret: &'a HashMap<usize, Ty>,
    params: &'a BTreeMap<u32, Ty>,
    qcast_ret: HashMap<String, Ty>,
    uf: &'a mut Uf,
}

impl Types<'_> {
    /// The type an already-rewritten node has, for the purpose of writing a signature.
    ///
    /// Deliberately shallower than inference: it reads types off things that *carry* one and answers
    /// [`Ty::Opaque`] for everything else, including arithmetic and `CASE`. Being wrong here
    /// declares a function over the wrong sort rather than asserting anything about the data, and
    /// `Opaque` is the fail-safe — it supports equality and nothing else, so a use that needed an
    /// order fails loudly instead of assuming one.
    fn of(&mut self, e: &Expr) -> Ty {
        match e {
            Expr::Nested(inner) => self.of(inner),
            Expr::Identifier(_) | Expr::CompoundIdentifier(_) => {
                match self.col.get(&nid(e)).cloned() {
                    Some(a) => self.uf.get_type(&a).unwrap_or(Ty::Opaque),
                    None => Ty::Opaque,
                }
            }
            Expr::Value(v) => crate::infer::literal_type(&v.value).unwrap_or(Ty::Opaque),
            Expr::Cast { data_type, .. } => {
                map_type_name(&data_type.to_string()).0.unwrap_or(Ty::Opaque)
            }
            Expr::Function(f) => {
                let name = obj_name(&f.name);
                builtin_rtype(&name)
                    .or_else(|| self.fn_ret.get(&nid(e)).copied())
                    .or_else(|| self.qcast_ret.get(&name.to_lowercase()).copied())
                    .or_else(|| {
                        let n: u32 = name.to_lowercase().strip_prefix("qp")?.parse().ok()?;
                        self.params.get(&n).copied()
                    })
                    .unwrap_or(Ty::Opaque)
            }
            _ => Ty::Opaque,
        }
    }
}

/// Build the `declare … function` table for the rewritten pair.
///
/// Three sources, matching the three kinds of symbol the preprocessor emits: one `qpN` per
/// parameter, one `qcastK` per shared conversion, and one entry per call whose result type nobody
/// declared. Keys are uppercased because that is how [`crate::catalog::parse_declare`] stores them
/// and how `lower.rs` looks them up — the two paths must agree or a declared return type silently
/// becomes the `INTEGER` default.
///
/// Must run *after* [`rewrite_casts`] and [`substitute_params`]: the `qcastK` return types are an
/// output of the rewrite, and a `qpN(0)` still spelled `$N` would contribute no call to declare.
pub fn declarations(
    queries: &[Query],
    inf: &mut Inferred,
    rw: &Rewrite,
) -> Result<HashMap<String, FnDecl>> {
    let mut out: HashMap<String, FnDecl> = HashMap::new();
    let decl = |t: Ty, aggregate: bool| FnDecl {
        ret: crate::types::normalize_type_name(t.sql()),
        aggregate,
    };
    for (n, t) in &inf.params {
        out.insert(format!("QP{n}"), decl(*t, false));
    }
    let mut qcast_ret: HashMap<String, Ty> = HashMap::new();
    for q in &rw.qcasts {
        out.insert(q.name.to_uppercase(), decl(q.ret, false));
        qcast_ret.insert(q.name.clone(), q.ret);
    }

    let Inferred { col, fn_ret, params, uf, .. } = inf;
    let mut ty = Types { col, fn_ret, params, qcast_ret, uf };

    // Every call the prover has no meaning for needs a signature, and the *same* symbol appearing
    // twice must get the same one -- two declarations of one name is not something the prover can
    // represent, so a genuine conflict is a refusal rather than last-writer-wins.
    let mut sigs: HashMap<String, Vec<Ty>> = HashMap::new();
    for q in queries {
        sweep(q, |e| {
            let Expr::Function(f) = e else { return Ok(()) };
            let Some(ret) = ty.fn_ret.get(&nid(e)).copied() else { return Ok(()) };
            let name = obj_name(&f.name);
            // Only positional value arguments, matching what `lower.rs` puts in the call's operand
            // list -- a declaration whose arity disagrees with the lowered call is exactly the kind
            // of mismatch the prover asserts on.
            let args: Vec<&Expr> = match &f.args {
                FunctionArguments::List(l) => l
                    .args
                    .iter()
                    .filter_map(|a| match a {
                        FunctionArg::Unnamed(FunctionArgExpr::Expr(x)) => Some(x),
                        _ => None,
                    })
                    .collect(),
                _ => Vec::new(),
            };
            // Nullary is refused rather than declared: the prover's DSL has no zero-argument form,
            // and giving it a filler argument would collapse two distinct calls into one constant.
            if args.is_empty() {
                return Err(unsupported(format!("nullary unknown function {name}")));
            }
            let argtypes: Vec<Ty> = args.iter().map(|a| ty.of(a)).collect();
            let key = name.to_uppercase();
            let d = decl(ret, is_agg_name(&name));
            if let Some(prev) = sigs.get(&key) {
                if *prev != argtypes || out.get(&key).is_some_and(|p| p.ret != d.ret) {
                    return Err(unsupported(format!(
                        "function {name} used with conflicting signatures"
                    )));
                }
            }
            sigs.insert(key.clone(), argtypes);
            out.insert(key, d);
            Ok(())
        })?;
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::infer::infer;
    use sqlparser::ast::Statement;
    use sqlparser::parser::Parser;

    /// Parse, infer, rewrite — and hand back the rendered queries plus what the rewrite produced.
    fn run(sql: &str) -> Result<(Vec<String>, Rewrite)> {
        let mut queries: Vec<Query> = Parser::parse_sql(&crate::DIALECT, sql)
            .expect("parses")
            .into_iter()
            .map(|s| match s {
                Statement::Query(q) => *q,
                other => panic!("not a query: {other}"),
            })
            .collect();
        let mut inf = infer(&queries, None)?;
        let mut rw = rewrite_casts(&mut queries, &mut inf, &Context::default())?;
        // As the pipeline does for a pair whose parameters line up.
        if let Some(mistyped) = rw.mistyped.take() {
            return Err(mistyped);
        }
        substitute_params(&mut queries, &mut inf)?;
        Ok((queries.iter().map(|q| q.to_string()).collect(), rw))
    }

    fn one(sql: &str) -> String {
        run(sql).expect("rewrites").0.remove(0)
    }

    #[test]
    fn a_cast_over_a_parameter_is_dropped() {
        // Rule 1. The parameter carries the type; the cast on it is the identity.
        let (q, rw) = run("SELECT * FROM t WHERE t.a = $1::integer").expect("rewrites");
        assert!(q[0].contains("qp1(0)"), "{}", q[0]);
        assert!(!q[0].to_uppercase().contains("CAST"), "{}", q[0]);
        assert!(rw.qcasts.is_empty(), "{:?}", rw.qcasts);
    }

    #[test]
    fn a_cast_over_a_literal_keeps_the_cast_and_retargets_it() {
        // Rules 2/3: `decimal` is retargeted to `numeric`, the spelling `Ty::Real` reads back as.
        let q = one("SELECT t.a, CAST(1 AS decimal) FROM t");
        assert!(q.to_uppercase().contains("CAST(1 AS NUMERIC)"), "{q}");
    }

    #[test]
    fn an_identity_cast_is_dropped_but_a_narrowing_one_is_not() {
        // Rule 4 against rule 5b. Both sides of the distinction in one pair: `name` infers VARCHAR,
        // so `::varchar` is a no-op and `::varchar(8)` is a truncation.
        let (q, rw) = run(
            "SELECT t.name::varchar FROM t; SELECT t.name::varchar(8) FROM t",
        )
        .expect("rewrites");
        assert_eq!(q[0], "SELECT t.name FROM t");
        assert!(q[1].contains("qcast0"), "{}", q[1]);
        assert_eq!(rw.qcasts, vec![QCast { name: "qcast0".into(), ret: Ty::Str }]);
    }

    #[test]
    fn two_different_truncations_are_not_one_symbol() {
        // The dedup key decides which conversions are *the same function*, so anything the target
        // says and the key drops becomes two operators sharing one symbol. A length qualifier says
        // where the string is cut: `varchar(8)` and `varchar(4)` are different functions of the
        // same column, and equating them would prove a rewrite that changes the cut.
        let (_, rw) =
            run("SELECT t.name::varchar(8) FROM t; SELECT t.name::varchar(4) FROM t")
                .expect("rewrites");
        assert_eq!(rw.qcasts.len(), 2, "two truncations collapsed to one symbol: {:?}", rw.qcasts);
    }

    #[test]
    fn widening_an_integer_to_a_real_stays_a_native_cast() {
        // Rule 5a: the prover understands this one, so it does not need a symbol. Only the target
        // is rewritten -- sqlparser keeps the `::` spelling, and `lower.rs` reads the target rather
        // than the spelling, so the two forms lower identically.
        let (q, rw) = run("SELECT t.user_id::numeric FROM t").expect("rewrites");
        assert!(q[0].to_uppercase().contains("T.USER_ID::NUMERIC"), "{}", q[0]);
        assert!(rw.qcasts.is_empty());
        // A float is not a REAL: it rounds. Its cast is a `qcast` into the opaque type.
        let (q, rw) = run("SELECT t.user_id::float FROM t").expect("rewrites");
        assert!(q[0].contains("qcast0"), "{}", q[0]);
        assert_eq!(rw.qcasts, vec![QCast { name: "qcast0".into(), ret: Ty::Opaque }]);
    }

    #[test]
    fn the_same_conversion_on_both_sides_shares_one_symbol() {
        // The point of the dedup key: a rewrite that leaves a conversion untouched must cancel, and
        // it only cancels if both sides name the same function.
        let (q, rw) = run(
            "SELECT t.name::integer FROM t; SELECT t.name::integer FROM t WHERE t.name = 'x'",
        )
        .expect("rewrites");
        assert!(q[0].contains("qcast0") && q[1].contains("qcast0"), "{q:?}");
        assert_eq!(rw.qcasts.len(), 1);
    }

    #[test]
    fn different_targets_over_one_operand_get_different_symbols() {
        let (_, rw) = run("SELECT t.name::integer, t.name::boolean FROM t").expect("rewrites");
        assert_eq!(rw.qcasts.len(), 2);
        assert_eq!(rw.qcasts[0].ret, Ty::Int);
        assert_eq!(rw.qcasts[1].ret, Ty::Bool);
    }

    #[test]
    fn parentheses_do_not_hide_the_operand() {
        // `($1)::int` is still rule 1, not "cast over unsupported operand Expr".
        let (q, rw) = run("SELECT * FROM t WHERE t.a = ($1)::integer").expect("rewrites");
        assert!(rw.qcasts.is_empty(), "{:?}", rw.qcasts);
        assert!(q[0].contains("qp1(0)"), "{}", q[0]);
        assert!(!q[0].to_uppercase().contains("CAST") && !q[0].contains("::"), "{}", q[0]);
    }

    #[test]
    fn an_untypeable_operand_takes_the_symbol_rather_than_the_refusal() {
        // Rules 4 and 5a are the only ones that read the operand's type, and both are unreachable
        // with `Ty::Opaque` -- `map_type_name` never yields `Some(Ty::Opaque)`, so the identity gate
        // cannot fire, and `Opaque != Int` kills the widening. So an operand nobody can type lands
        // on 5b, which names the conversion instead of dropping or retargeting it.
        let (q, rw) = run("SELECT (t.a + 1)::integer FROM t").expect("rewrites");
        assert_eq!(q[0], "SELECT qcast0(t.a + 1) FROM t");
        assert_eq!(rw.qcasts, vec![QCast { name: "qcast0".into(), ret: Ty::Int }]);
    }

    #[test]
    fn a_cast_states_its_own_type_for_the_cast_above_it() {
        // A nested cast is the *most* precise operand there is: the SQL says what it produces. Here
        // that makes the outer a real conversion (VARCHAR from INTEGER), so both casts survive as
        // symbols -- the inner because `name` infers VARCHAR, the outer because Str != Int.
        // `decide` sweeps top-down, so the *outer* cast is numbered first.
        let (q, rw) = run("SELECT t.name::integer::varchar FROM t").expect("rewrites");
        assert_eq!(q[0], "SELECT qcast0(qcast1(t.name)) FROM t");
        assert_eq!(rw.qcasts.len(), 2, "{:?}", rw.qcasts);
        assert_eq!(rw.qcasts[0].ret, Ty::Str, "outer target");
        assert_eq!(rw.qcasts[1].ret, Ty::Int, "inner target");
        // The point of the arm: the outer read `Int` off the inner's target, not off `name`. Read
        // off `name` it would be VARCHAR to VARCHAR, an identity, and the outer would be gone.
    }

    #[test]
    fn chained_identity_casts_walk_the_column_binding_to_its_final_home() {
        // Both casts are no-ops over an INTEGER column, so both hoist. `Apply` pushes
        // `(operand, inner slot)` before `(inner slot, outer slot)` and `rewrite_casts` replays them
        // in that order, so the `col` entry follows the chain. Push order *is* the argument -- swap
        // the two and the binding is dropped instead of moved.
        let mut queries: Vec<Query> = Parser::parse_sql(
            &crate::DIALECT,
            "SELECT t.user_id::integer::integer FROM t",
        )
        .expect("parses")
        .into_iter()
        .map(|s| match s {
            Statement::Query(q) => *q,
            other => panic!("not a query: {other}"),
        })
        .collect();
        let mut inf = infer(&queries, None).expect("infers");
        let rw = rewrite_casts(&mut queries, &mut inf, &Context::default()).expect("rewrites");
        assert_eq!(queries[0].to_string(), "SELECT t.user_id FROM t");
        assert!(rw.qcasts.is_empty(), "{:?}", rw.qcasts);
        // The surviving node is the one the outer cast used to occupy, and inference still knows
        // which column it is.
        let mut found = 0;
        let _ = sqlparser::ast::visit_expressions(&queries[0], |e| {
            if matches!(e, Expr::CompoundIdentifier(_)) && inf.col.contains_key(&nid(e)) {
                found += 1;
            }
            ControlFlow::<()>::Continue(())
        });
        assert_eq!(found, 1, "the column binding did not survive the chained hoist");
    }

    #[test]
    fn a_widening_cast_over_a_truncating_one_is_still_an_identity() {
        // `varchar(8)` produces a VARCHAR, so casting that to unqualified `varchar` is a no-op and
        // rule 4 takes the *outer*. The truncation underneath must not be touched.
        let (q, rw) = run("SELECT t.name::varchar(8)::varchar FROM t").expect("rewrites");
        assert_eq!(q[0], "SELECT qcast0(t.name) FROM t", "the truncation was dropped");
        assert_eq!(rw.qcasts.len(), 1);
    }

    #[test]
    fn a_qualified_target_over_a_cast_is_not_an_identity() {
        // The mirror: `varchar` then `varchar(8)` cuts the string, so the outer has to survive even
        // though both sides read VARCHAR. Rule 4's `!qualified` condition is what saves it.
        let (q, rw) = run("SELECT t.name::varchar::varchar(8) FROM t").expect("rewrites");
        assert_eq!(q[0], "SELECT qcast0(t.name) FROM t", "the inner no-op should still go");
        assert_eq!(rw.qcasts.len(), 1, "{:?}", rw.qcasts);
        assert_eq!(rw.qcasts[0].ret, Ty::Str);
    }

    #[test]
    fn a_cast_over_a_modelled_call_no_longer_refuses() {
        // The operand arm used to be gated on `is_opaque_call`, which excluded exactly the functions
        // whose return type is known. An aggregate is the common case in this corpus.
        let (q, rw) = run("SELECT sum(t.user_id)::numeric FROM t").expect("rewrites");
        assert!(q[0].contains("qcast0(sum(t.user_id))"), "{}", q[0]);
        assert_eq!(rw.qcasts.len(), 1);
    }

    #[test]
    fn an_array_or_tuple_operand_still_refuses() {
        // Not conservatism for its own sake: under `= ANY` a `qcast` around an array literal would
        // cost `lower_quantified`'s exact OR-expansion of it, and `lower_expr` has no arm for a
        // tuple, so a `qcast` around one would move the refusal rather than lift it.
        let e = run("SELECT t.a FROM t WHERE t.a = ANY(ARRAY[$1]::bigint[])")
            .unwrap_err()
            .to_string();
        assert!(e.contains("cast over unsupported operand Array"), "{e}");
        let e = run("SELECT (t.a, t.b)::record FROM t").unwrap_err().to_string();
        assert!(e.contains("cast over unsupported operand Tuple"), "{e}");
    }

    #[test]
    fn an_unmappable_target_becomes_an_opaque_carrier() {
        // The conversion is still a function of its operand; only its meaning is out of reach. So it
        // gets a symbol with an equality-only sort on both ends rather than refusing the pair, and two
        // sides that agree on the cast cancel it.
        let (q, rw) = run("SELECT t.a::jsonb FROM t").expect("rewrites");
        assert_eq!(q[0], "SELECT qcast0(t.a) FROM t");
        assert_eq!(rw.qcasts.len(), 1);
        assert_eq!(data_type(rw.qcasts[0].ret).to_string(), "VARBINARY");
        assert_eq!(rw.qcasts[0].ret, Ty::Opaque);
    }

    #[test]
    fn two_different_unmappable_targets_are_not_one_symbol() {
        // Every unmappable target carries as `Opaque`, so the sorts alone cannot tell `jsonb`
        // from `uuid[]` -- and a Z3 uninterpreted function *is* its name plus its sorts. If these
        // shared a symbol the pair would prove that converting a column to JSON and to a UUID array
        // give the same value.
        let (q, rw) = run("SELECT t.a::jsonb FROM t; SELECT t.a::uuid[] FROM t").expect("rewrites");
        assert_eq!(rw.qcasts.len(), 2, "two targets collapsed to one symbol: {:?}", rw.qcasts);
        assert_ne!(q[0], q[1]);
    }

    #[test]
    fn an_unmappable_cast_is_never_an_identity() {
        // The identity rule drops a cast whose target the operand already has. Both sides here read
        // `Opaque` -- an unknown column, an unreadable target -- and letting them match would drop the
        // cast outright, proving `a::jsonb` equal to a bare `a`.
        let q = one("SELECT a::jsonb FROM t");
        assert_eq!(q, "SELECT qcast0(a) FROM t", "an unreadable cast was dropped as an identity");
    }

    #[test]
    fn two_spellings_of_one_target_still_share_a_symbol() {
        // The point of deduping in the first place: a pair whose two sides merely respell a cast has
        // to cancel it, so a mappable target collapses to its type and `text` meets `varchar`.
        let (q, rw) =
            run("SELECT t.n::text FROM t; SELECT t.n::varchar FROM t").expect("rewrites");
        assert_eq!(rw.qcasts.len(), 1, "{:?}", rw.qcasts);
        assert_eq!(q[0], q[1]);
    }

    /// Parse, infer, rewrite, substitute and synthesize — the whole tail of the preprocessor.
    fn decls(sql: &str) -> Result<HashMap<String, FnDecl>> {
        let mut queries: Vec<Query> = Parser::parse_sql(&crate::DIALECT, sql)
            .expect("parses")
            .into_iter()
            .map(|s| match s {
                Statement::Query(q) => *q,
                other => panic!("not a query: {other}"),
            })
            .collect();
        let mut inf = infer(&queries, None)?;
        let rw = rewrite_casts(&mut queries, &mut inf, &Context::default())?;
        substitute_params(&mut queries, &mut inf)?;
        declarations(&queries, &mut inf, &rw)
    }

    #[test]
    fn every_synthesized_symbol_is_declared() {
        // One of each: a parameter, a conversion, and a call nobody modelled.
        let d = decls("SELECT q_op_f(t.name::integer) FROM t WHERE t.user_id = $1").expect("ok");
        assert_eq!(d["QP1"].ret, "INTEGER");
        assert_eq!(d["QCAST0"].ret, "INTEGER");
        assert_eq!(d["Q_OP_F"].ret, "VARBINARY");
        assert!(!d["Q_OP_F"].aggregate);
    }

    #[test]
    fn an_aggregate_symbol_is_declared_as_an_aggregate() {
        // The `qa_` prefix is the only thing separating an aggregate from a scalar here, and
        // getting it wrong turns one output row into one row per input row.
        let d = decls("SELECT qa_int_total(t.user_id) FROM t").expect("ok");
        assert_eq!(d["QA_INT_TOTAL"].ret, "INTEGER");
        assert!(d["QA_INT_TOTAL"].aggregate);
    }

    #[test]
    fn the_declared_return_type_matches_what_parse_declare_would_read() {
        // `Ty::Real` spells itself DOUBLE, but the DSL parser normalises DOUBLE to REAL. If these
        // two paths disagree the same pair gets different IR depending on which produced it.
        let d = decls("SELECT q_op_f(t.amount) FROM t WHERE t.amount = 1.5").expect("ok");
        let (_, parsed) =
            crate::catalog::parse_declare("declare scalar function f(double) returns double;")
                .expect("parses");
        assert_eq!(d["Q_OP_F"].ret, "VARBINARY");
        assert_eq!(parsed.ret, "REAL");
        assert_eq!(crate::types::normalize_type_name(Ty::Real.sql()), parsed.ret);
    }

    #[test]
    fn one_symbol_used_two_ways_refuses_the_pair() {
        let e = decls("SELECT q_op_f(t.user_id), q_op_f(t.name) FROM t").unwrap_err().to_string();
        assert!(e.contains("conflicting signatures"), "{e}");
    }

    #[test]
    fn a_nullary_unknown_function_refuses_the_pair() {
        let e = decls("SELECT t.a, q_int_seq() FROM t").unwrap_err().to_string();
        assert!(e.contains("nullary unknown function"), "{e}");
    }

    #[test]
    fn a_rewritten_operand_keeps_its_attribution() {
        // The re-keying in `rewrite_casts`. `t.name::varchar` is an identity cast, so `t.name` moves
        // into the cast's slot; if its entry were not moved with it, the signature synthesis that
        // runs next would type the column Opaque instead of VARCHAR.
        let sql = "SELECT q_op_f(t.name::varchar) FROM t";
        let mut queries: Vec<Query> = Parser::parse_sql(&crate::DIALECT, sql)
            .expect("parses")
            .into_iter()
            .map(|s| match s {
                Statement::Query(q) => *q,
                other => panic!("not a query: {other}"),
            })
            .collect();
        let mut inf = infer(&queries, None).expect("infers");
        rewrite_casts(&mut queries, &mut inf, &Context::default()).expect("rewrites");
        let found = inf
            .col
            .values()
            .any(|a| matches!(a, Atom::Col(t, c) if t == "t" && c == "name"));
        assert!(found, "the hoisted column lost its attribution: {:?}", inf.col);
    }

    #[test]
    fn a_type_key_is_one_for_the_spellings_of_one_type_and_no_others() {
        let k = type_key;
        // One array type per element type, whatever the dimensions written; and the classes
        // inference has are one key each, as they are to `canon_target`.
        assert_eq!(k("bigint[]"), k("INT8[]"));
        assert_eq!(k("bigint[]"), k("integer[]"));
        assert_eq!(k("text[]"), k("varchar[]"));
        assert_eq!(k("int[][]"), k("int[]"));
        assert_eq!(k("int[3]"), k("int[]"));
        assert_eq!(k("varchar(3)"), k("text"));
        // An array is not its element, and two types with no `Ty` are two.
        assert_ne!(k("bigint"), k("bigint[]"));
        assert_ne!(k("uuid"), k("uuid[]"));
        assert_ne!(k("json"), k("jsonb"));
        assert_ne!(k("uuid"), k("jsonb"));
        assert_eq!(k("float8"), k("DOUBLE PRECISION"));
        assert_eq!(k("float4"), k("real"));
        assert_ne!(k("float8"), k("real"));
        // A quoted name is the name as written.
        assert_ne!(k("\"MyType\""), k("mytype"));
        assert_eq!(k("MyType"), k("mytype"));
        assert_ne!(k("\"bit\""), k("bit"));
        assert_ne!(k("\"mood\""), k("mood"));

        // A key has no typmod: a parameter's type has none.
        assert_eq!(k("bit(3)"), k("bit(5)"));
        assert_eq!(k("bit(3)"), k("bit"));
        assert_eq!(k("varbit(5)"), k("varbit"));
        assert_eq!(k("timetz(0)"), k("timetz"));
        assert_eq!(k("time (0) with time zone"), k("time with time zone"));
        assert_eq!(k("geometry(Point,4326)"), k("geometry"));
        assert_ne!(k("bit"), k("varbit"));
        // `float(p)` picks the type by its precision.
        assert_eq!(k("float(24)"), k("real"));
        assert_eq!(k("float(25)"), k("double precision"));
        assert_eq!(k("float"), k("float8"));
        // Two names the grammar gives one type.
        assert_eq!(k("time with time zone"), k("timetz"));
        assert_eq!(k("bit varying"), k("varbit"));
        assert_eq!(k("bit varying(5)[]"), k("varbit[]"));
        assert_ne!(k("time with time zone"), k("time"));
        // The SQL-standard array spelling is the array.
        assert_eq!(k("uuid ARRAY"), k("uuid[]"));
        assert_eq!(k("varchar(3) ARRAY[4]"), k("text[]"));
        assert_ne!(k("uuid ARRAY"), k("uuid"));
        // `pg_catalog.T` is `T` for a type Postgres predefines, and for no other.
        assert_eq!(k("pg_catalog.uuid"), k("uuid"));
        assert_eq!(k("pg_catalog.int4"), k("integer"));
        assert_eq!(k("pg_catalog.float8[]"), k("double precision[]"));
        assert_ne!(k("pg_catalog.mood"), k("mood"));
        // `public.T` is `T` for a type the DDL creates, and for no name Postgres or this crate
        // gives a meaning, where the bare name is `pg_catalog`'s type.
        assert_eq!(k("public.mood"), k("mood"));
        assert_eq!(k("public.mood[]"), k("mood[]"));
        assert_ne!(k("public.uuid"), k("uuid"));
        assert_ne!(k("public.text"), k("text"));
        assert_ne!(k("public.integer"), k("integer"));
        assert_ne!(k("public.real"), k("real"));
        assert_ne!(k("app.mood"), k("mood"));
        assert_ne!(k("public.\"Mood\""), k("mood"));
        assert_ne!(k("\"public\".mood"), k("mood"));

        for typmod in [
            "varchar(1)[]", "varchar(3)", "numeric(10,2)[]", "bit(3)", "varbit(5)", "timetz(0)",
            "time(0) with time zone", "geometry(Point,4326)", "varchar(3) ARRAY", "interval day",
            // An unquoted `bit` is `bit(1)`, alone or as an array's element.
            "bit", "BIT", "bit[]", "bit ARRAY",
        ] {
            assert!(has_typmod(typmod), "{typmod}");
        }
        for plain in ["varchar[]", "uuid", "bigint[]", "varbit", "timetz", "\"bit\"", "int ARRAY", "bit varying", "interval"] {
            assert!(!has_typmod(plain), "{plain}");
        }
    }

    #[test]
    fn a_typmod_on_a_type_with_no_ty_keeps_the_cast() {
        // #298's review. `$1::bit(3)` truncates and `$1::timetz(0)` rounds, as `$1::varchar(3)`
        // does, and `main` kept them through the flag `map_type_name` reports. Asking only an
        // array about its element dropped them like `$1::uuid`. An unquoted `bit` is `bit(1)`, and
        // `varchar(3) ARRAY` is `varchar(3)[]`.
        for target in ["bit(3)", "timetz(0)", "varbit(5)", "bit", "bit[]", "varchar(3) ARRAY"] {
            let sql = format!("SELECT t.id FROM t WHERE t.b = $1::{target}; SELECT t.id FROM t WHERE t.b = $1");
            let (q, rw) = run(&sql).expect(&sql);
            assert!(q[0].contains("qcast0") && !q[1].contains("qcast"), "{sql}: {q:?}");
            assert_eq!(rw.qcasts.len(), 1, "{sql}: {:?}", rw.qcasts);
        }
        // Two lengths are two functions, and the type with no length is none.
        let (_, rw) = run("SELECT t.id FROM t WHERE t.b = $1::bit(3); SELECT t.id FROM t WHERE t.b = $1::bit(4)")
            .expect("rewrites");
        assert_eq!(rw.qcasts.len(), 2, "{:?}", rw.qcasts);
        let (q, rw) = run("SELECT t.id FROM t WHERE t.b = $1::varbit; SELECT t.id FROM t WHERE t.b = $1")
            .expect("rewrites");
        assert!(rw.qcasts.is_empty() && q[0] == q[1], "{q:?}");
    }

    #[test]
    fn an_in_list_states_a_type_only_where_no_item_can_widen_it() {
        // #298's review. Postgres resolves one common type over the subject and the items, so a
        // typed item decides what the bare parameters beside it are. No DDL here: the column is of
        // whatever type fits, and the list is of the type its items state.
        for sql in [
            // The cast sibling makes `$1` a `double precision`, and `$1::real` a conversion.
            "SELECT t.id FROM t WHERE t.r IN ($1, $2::float8) AND t.x = $1::real; \
             SELECT t.id FROM t WHERE t.r IN ($1, $2::float8) AND t.x = $1",
            // A sibling parameter another use typed does the same: the list's parameters are one
            // type between them.
            "SELECT t.id FROM t WHERE t.f = $2::float8 AND t.r IN ($1, $2) AND t.x = $1::real; \
             SELECT t.id FROM t WHERE t.f = $2::float8 AND t.r IN ($1, $2) AND t.x = $1",
            // Through a second list.
            "SELECT t.id FROM t WHERE t.r IN ($1, $2) AND t.s IN ($2, $3) AND t.x = $1::real AND t.y = $3::float8; \
             SELECT t.id FROM t WHERE t.r IN ($1, $2) AND t.s IN ($2, $3) AND t.x = $1 AND t.y = $3",
            // An item that is none of the readable kinds.
            "SELECT t.id FROM t WHERE t.r IN ($1, abs(t.z)) AND t.x = $1::uuid; \
             SELECT t.id FROM t WHERE t.r IN ($1, abs(t.z)) AND t.x = $1",
            "SELECT t.id FROM t WHERE t.r IN ($1, B'101') AND t.x = $1::varbit; \
             SELECT t.id FROM t WHERE t.r IN ($1, B'101') AND t.x = $1",
        ] {
            // Of `$1`, or of the sibling it is read together with.
            let err = run(sql).expect_err(sql).to_string();
            assert!(err.contains("but not every use of $") && err.contains("states that type"), "{sql}: {err}");
        }
        for sql in [
            "SELECT t.id FROM t WHERE t.u IN ($1, $2) AND t.v = $1::uuid; \
             SELECT t.id FROM t WHERE t.u IN ($1, $2) AND t.v = $1",
            // Items with no type of their own, and one of the type the cast states.
            "SELECT t.id FROM t WHERE t.u IN ($1, NULL, 'a0eebc99-9c0b-4ef8-bb6d-6bb9bd380a11', ($2)) AND t.v = $1::uuid; \
             SELECT t.id FROM t WHERE t.u IN ($1, NULL, 'a0eebc99-9c0b-4ef8-bb6d-6bb9bd380a11', ($2)) AND t.v = $1",
            "SELECT t.id FROM t WHERE t.u IN ($1, $2::uuid) AND t.v = $1::uuid; \
             SELECT t.id FROM t WHERE t.u IN ($1, $2::uuid) AND t.v = $1",
            "SELECT t.id FROM t WHERE t.u IN ($1, t.w) AND t.v = $1::uuid; \
             SELECT t.id FROM t WHERE t.u IN ($1, t.w) AND t.v = $1",
        ] {
            let (q, rw) = run(sql).expect(sql);
            assert!(rw.qcasts.is_empty() && q[0] == q[1], "{sql}: {q:?}");
        }
    }

    #[test]
    fn pooled_groups_are_closed_under_a_shared_member() {
        let mut found: HashMap<u32, u32> = [(1, 1), (2, 5), (3, 2), (4, 7), (5, 0), (6, 9)].into();
        pool(&[vec![1, 2], vec![6], vec![4, 5], vec![2, 3]], &mut found, |a, b| *a.max(b));
        let mut got: Vec<(u32, u32)> = found.into_iter().collect();
        got.sort_unstable();
        assert_eq!(got, [(1, 5), (2, 5), (3, 5), (4, 7), (5, 7), (6, 9)]);
    }

    #[test]
    fn a_null_test_types_nothing() {
        // #298's review: `$1 IS NULL` is no use that states another type, or none.
        for sql in [
            "SELECT t.id FROM t WHERE (t.u = $1::uuid OR $1 IS NULL); SELECT t.id FROM t WHERE (t.u = $1 OR $1 IS NULL)",
            "SELECT t.id FROM t WHERE t.u = $1::uuid AND ($1) IS NOT NULL; SELECT t.id FROM t WHERE t.u = $1 AND ($1) IS NOT NULL",
        ] {
            let (q, rw) = run(sql).expect(sql);
            assert!(rw.qcasts.is_empty() && q[0] == q[1], "{sql}: {q:?}");
        }
    }

    #[test]
    fn a_cast_to_a_type_with_no_ty_is_refused_unless_every_use_states_it() {
        // #285. Each pair is one plan with the cast dropped, and Postgres tells its two sides
        // apart: it types `$1` at its first use and converts at a cast to another type.
        for sql in [
            // An array is not its element.
            "SELECT t.id FROM t WHERE t.n = ($1)::bigint[]; SELECT t.id FROM t WHERE t.n = ($1)::bigint",
            "SELECT t.id, ($1)::bigint[] AS x FROM t; SELECT t.id, ($1)::bigint AS x FROM t",
            // Two types with no `Ty`.
            "SELECT t.id, ($1)::jsonb AS x FROM t; SELECT t.id, ($1)::uuid AS x FROM t",
            "SELECT t.id FROM t WHERE t.j = $1::json; SELECT t.id FROM t WHERE t.j = $1::jsonb",
            "SELECT t.id, ($1)::bigint[] AS x FROM t; SELECT t.id, ($1)::text[] AS x FROM t",
            // A use that states no type: a bare select item is `text`.
            "SELECT t.id, ($1)::int4range AS x FROM t; SELECT t.id, $1 AS x FROM t",
            // One query, two casts: the first use decides, and the two orders are two statements.
            "SELECT t.id FROM t WHERE t.n = ANY($1::int[]) AND t.name = ANY($1::text[]); \
             SELECT t.id FROM t WHERE t.name = ANY($1::text[]) AND t.n = ANY($1::int[])",
        ] {
            let err = run(sql).expect_err(sql).to_string();
            assert!(err.contains("not every use of $1 states that type"), "{sql}: {err}");
        }
    }

    #[test]
    fn a_cast_every_use_of_the_parameter_states_is_still_dropped() {
        for sql in [
            // Cast wherever it appears, in any spelling of the one type.
            "SELECT t.id FROM t WHERE t.u = $1::uuid; SELECT t.id FROM t WHERE $1::UUID = t.u",
            "SELECT t.id FROM t WHERE t.a = ANY($1::bigint[]); SELECT t.id FROM t WHERE t.a = ANY($1::integer[])",
            "SELECT t.id FROM t WHERE t.s = ANY($1::text[]); SELECT t.id FROM t WHERE t.s = ANY($1::varchar[])",
            // Compared with a column no DDL declares and nothing else types: the column is what
            // fits, so the use agrees.
            "SELECT t.id FROM t WHERE t.u = $1::uuid; SELECT t.id FROM t WHERE t.u = $1",
            "SELECT t.id FROM t WHERE t.u = ANY($1::uuid[]); SELECT t.id FROM t WHERE t.u = ANY($1)",
            "SELECT t.id FROM t WHERE t.u IN ($1::uuid, $2::uuid); SELECT t.id FROM t WHERE t.u IN ($1, $2)",
        ] {
            let (q, rw) = run(sql).expect(sql);
            assert!(rw.qcasts.is_empty(), "{sql}: {:?}", rw.qcasts);
            assert!(!q.join(" ").to_uppercase().contains("CAST"), "{sql}: {q:?}");
        }
    }

    #[test]
    fn a_column_inference_typed_is_evidence_where_no_ddl_declares_it() {
        // `t.a = 1` makes `a` an integer, so the bare `$1` of `t.a = ANY($1)` is an array of
        // integers, and `$1::uuid[]` on the other side is a cast to another type. Inference reads
        // nothing off `= ANY($N)`, so its own test of the parameter's type does not see this.
        let sql = "SELECT t.id FROM t WHERE t.a = 1 AND t.a = ANY($1); \
                   SELECT t.id FROM t WHERE t.a = 1 AND t.a = ANY($1::uuid[])";
        let err = run(sql).expect_err(sql).to_string();
        assert!(err.contains("not every use of $1 states that type"), "{err}");
    }

    #[test]
    fn a_typmod_on_an_arrays_element_keeps_the_cast() {
        // `$1::varchar(1)[]` truncates each element. The flag `map_type_name` reports is never set
        // for an array, so this was dropped like `$1::text[]`, and the pair was one plan.
        let (q, rw) = run(
            "SELECT t.id FROM t WHERE t.s = ANY($1::varchar(1)[]); SELECT t.id FROM t WHERE t.s = ANY($1::text[])",
        )
        .expect("rewrites");
        assert!(q[0].contains("qcast0") && !q[1].contains("qcast"), "{q:?}");
        assert_eq!(rw.qcasts.len(), 1, "{:?}", rw.qcasts);
        // Two lengths are two functions.
        let (_, rw) = run(
            "SELECT t.id FROM t WHERE t.s = ANY($1::varchar(1)[]); SELECT t.id FROM t WHERE t.s = ANY($1::varchar(2)[])",
        )
        .expect("rewrites");
        assert_eq!(rw.qcasts.len(), 2, "{:?}", rw.qcasts);
    }

    #[test]
    fn a_cast_beside_a_use_that_states_another_type_is_refused_whatever_the_target() {
        // #299. A bare parameter among the items of a select list is a `text`, so a cast of it to
        // anything else on the other side is a cast to another type. A subquery's list is no
        // different: Postgres makes the item of `t.a IN (SELECT $1)` a `text` once it has read the
        // subquery, whatever it is then compared with, so over an integer `a` that statement does
        // not even prepare (`operator does not exist: integer = text`).
        for sql in [
            "SELECT t.id, ($1)::bigint AS x FROM t; SELECT t.id, $1 AS x FROM t",
            "SELECT t.id, $1::timestamptz AS x FROM t; SELECT t.id, $1 AS x FROM t",
            "SELECT t.id FROM t WHERE t.a IN (SELECT $1::bigint); SELECT t.id FROM t WHERE t.a IN (SELECT $1)",
            "SELECT t.id FROM t, (SELECT $1::int AS x) AS s WHERE t.a = s.x; SELECT t.id FROM t, (SELECT $1 AS x) AS s WHERE t.a = s.x",
            "SELECT t.id FROM t WHERE EXISTS (SELECT $1::int); SELECT t.id FROM t WHERE EXISTS (SELECT $1)",
        ] {
            let err = run(sql).expect_err(sql).to_string();
            assert!(err.contains("not every use of $1 states that type"), "{sql}: {err}");
        }
        // The same use, stating the cast's own type; and a comparison with a cast, whose target is
        // what the parameter is there.
        for sql in [
            "SELECT t.id, $1::text AS x FROM t; SELECT t.id, $1 AS x FROM t",
            "SELECT t.id, $1::varchar AS x FROM t; SELECT t.id, $1 AS x FROM t",
            "SELECT t.id FROM t WHERE t.s IN (SELECT $1::text); SELECT t.id FROM t WHERE t.s IN (SELECT $1)",
            "SELECT t.id FROM t WHERE t.ts::date = $1::date; SELECT t.id FROM t WHERE t.ts::date = $1",
            // A branch of a set operation takes its type from the other branch's item.
            "SELECT $1::bigint AS x UNION ALL SELECT t.a::bigint FROM t; SELECT $1 AS x UNION ALL SELECT t.a::bigint FROM t",
            "SELECT t.id, 'x' FROM t UNION ALL SELECT 1, $1::text; SELECT t.id, 'x' FROM t UNION ALL SELECT 1, $1",
            "SELECT t.id, $1::text FROM t UNION SELECT 1, $2::text; SELECT t.id, $1 FROM t UNION SELECT 1, $2",
        ] {
            // The cast over the parameter is gone, so the two sides are one query.
            let (q, _) = run(sql).expect(sql);
            assert_eq!(q[0], q[1], "{sql}");
        }
        // And a branch whose other branch's item is of another type, or is not read.
        for sql in [
            "SELECT $1::text AS x UNION ALL SELECT t.a::bigint FROM t; SELECT $1 AS x UNION ALL SELECT t.a::bigint FROM t",
            "SELECT t.id, $1::bigint FROM t UNION ALL SELECT t.id, 'a' FROM t; SELECT t.id, $1 FROM t UNION ALL SELECT t.id, 'a' FROM t",
            "SELECT $1::int AS x UNION ALL SELECT abs(t.a) FROM t; SELECT $1 AS x UNION ALL SELECT abs(t.a) FROM t",
            // Three branches are resolved pair by pair from the left, which is not read.
            "SELECT t.id, $1::int FROM t UNION ALL SELECT t.id, 1 FROM t UNION ALL SELECT t.id, 2 FROM t; \
             SELECT t.id, $1 FROM t UNION ALL SELECT t.id, 1 FROM t UNION ALL SELECT t.id, 2 FROM t",
            // A branch in parentheses is a query of its own, and still no plain select list.
            "(SELECT t.id, $1::int FROM t) UNION ALL (SELECT t.id, 1 FROM t); (SELECT t.id, $1 FROM t) UNION ALL (SELECT t.id, 1 FROM t)",
        ] {
            let err = run(sql).expect_err(sql).to_string();
            assert!(err.contains("not every use of $1 states that type"), "{sql}: {err}");
        }
    }

    #[test]
    fn a_cast_is_dropped_only_where_every_use_of_the_parameter_is_read() {
        // #302's review. For a target inference has a class for, a use that states no type used
        // to leave the decision to inference, whose type for the parameter is not Postgres's
        // where the use is one it reads nothing off. Each pair here was one plan, and Postgres
        // tells its sides apart: `abs($1)` makes `$1` a `double precision`, `$1 = '01'` a `text`.
        for sql in [
            "SELECT t.id FROM t WHERE abs($1) < 1 AND t.a = $1::int; SELECT t.id FROM t WHERE abs($1) < 1 AND t.a = $1",
            "SELECT t.id FROM t WHERE $1 = '01' AND t.a = $1::int; SELECT t.id FROM t WHERE $1 = '1' AND t.a = $1::int",
            "SELECT t.id FROM t WHERE t.a + $1 > 0 AND t.b = $1::int; SELECT t.id FROM t WHERE t.a + $1 > 0 AND t.b = $1",
            "SELECT t.id FROM t WHERE coalesce($1, t.a) > 0 AND t.b = $1::int; SELECT t.id FROM t WHERE coalesce($1, t.a) > 0 AND t.b = $1",
            "SELECT t.id FROM t WHERE CASE WHEN t.a > 0 THEN $1 ELSE t.b END > 0 AND t.b = $1::int; \
             SELECT t.id FROM t WHERE CASE WHEN t.a > 0 THEN $1 ELSE t.b END > 0 AND t.b = $1",
            // Read, and of another type than the cast's.
            "SELECT t.id FROM t WHERE t.s LIKE $1 AND t.a = $1::int; SELECT t.id FROM t WHERE t.s LIKE $1 AND t.a = $1",
            "SELECT t.id FROM t WHERE $1 > 5 AND t.s = $1::text; SELECT t.id FROM t WHERE $1 > 5 AND t.s = $1",
            "SELECT t.id FROM t WHERE $1 = 2.5 AND t.a = $1::int; SELECT t.id FROM t WHERE $1 = 2.5 AND t.a = $1",
            "SELECT t.id FROM t WHERE $1 IN ('a', 'b') AND t.a = $1::int; SELECT t.id FROM t WHERE $1 IN ('a', 'b') AND t.a = $1",
            "SELECT t.id FROM t WHERE $1 BETWEEN 1 AND 10 AND t.s = $1::text; SELECT t.id FROM t WHERE $1 BETWEEN 1 AND 10 AND t.s = $1",
            "SELECT t.id FROM t WHERE $1 > DATE '2020-01-01' AND t.a = $1::int; SELECT t.id FROM t WHERE $1 > DATE '2020-01-01' AND t.a = $1",
            // A subject whose items, or bounds, are not of one type between them.
            "SELECT t.id FROM t WHERE $1 IN (1, 'a') AND t.a = $1::int; SELECT t.id FROM t WHERE $1 IN (1, 'a') AND t.a = $1",
            "SELECT t.id FROM t WHERE $1 BETWEEN 1 AND 2.5 AND t.a = $1::int; SELECT t.id FROM t WHERE $1 BETWEEN 1 AND 2.5 AND t.a = $1",
            // `B'..'` and `N'..'` are typed constants this does not read.
            "SELECT t.id FROM t WHERE $1 = B'101' AND t.a = $1::int; SELECT t.id FROM t WHERE $1 = B'101' AND t.a = $1",
            // Two parameters compared with each other are one type between them: here `$2` is
            // whatever `t.u = $1` makes `$1`, a `uuid`, and `$2::jsonb` converts it.
            "SELECT t.id FROM t WHERE t.u = $1::uuid AND $1 = $2 AND t.j = $2::jsonb; \
             SELECT t.id FROM t WHERE t.u = $1::uuid AND $1 = $2 AND t.j = $2",
            "SELECT t.id FROM t WHERE $1 = $2 AND t.a = $1::int; SELECT t.id FROM t WHERE $1 = $2 AND t.a = $1",
            // Two rows of different lengths, and a row against something that is not one.
            "SELECT t.id FROM t WHERE (t.u, t.a) = ($1::uuid, $2, 1); SELECT t.id FROM t WHERE (t.u, t.a) = ($1, $2, 1)",
            // The argument of `date`, which Postgres makes a `timestamptz`; and any other call.
            "SELECT t.id FROM t WHERE date(t.ts) = date($1) AND t.d = $1::date; SELECT t.id FROM t WHERE date(t.ts) = date($1) AND t.d = $1",
            "SELECT t.id FROM t WHERE date_trunc('day', t.ts) = $1 AND t.d = $1::date; \
             SELECT t.id FROM t WHERE date_trunc('day', t.ts) = $1 AND t.d = $1",
            "SELECT t.id FROM t WHERE date(t.ts, 1) = $1 AND t.d = $1::date; SELECT t.id FROM t WHERE date(t.ts, 1) = $1 AND t.d = $1",
        ] {
            let err = run(sql).expect_err(sql).to_string();
            assert!(err.contains("not every use of $1 states that type"), "{sql}: {err}");
        }
    }

    #[test]
    fn each_use_that_is_read_leaves_a_cast_to_its_type_dropped() {
        // The cast side against the bare side, for each use Postgres has a plain rule for.
        for sql in [
            // A constant: an integer, a `numeric`, a boolean, and `text` for a string, since two
            // operands of unknown type are compared as text.
            "SELECT t.id FROM t WHERE $1::int = 5 AND t.id = $1; SELECT t.id FROM t WHERE $1 = 5 AND t.id = $1",
            "SELECT t.id FROM t WHERE $1::bigint > -5; SELECT t.id FROM t WHERE $1 > -5",
            "SELECT t.id FROM t WHERE $1::numeric < 2.5; SELECT t.id FROM t WHERE $1 < 2.5",
            "SELECT t.id FROM t WHERE $1::boolean = true; SELECT t.id FROM t WHERE $1 = true",
            "SELECT t.id FROM t WHERE $1::text = 'x'; SELECT t.id FROM t WHERE $1 = 'x'",
            "SELECT t.id FROM t WHERE $1::date > DATE '2020-01-01'; SELECT t.id FROM t WHERE $1 > DATE '2020-01-01'",
            // `LIKE` and its kin, either way round.
            "SELECT t.id FROM t WHERE t.s LIKE $1::text; SELECT t.id FROM t WHERE t.s LIKE $1",
            "SELECT t.id FROM t WHERE t.s NOT ILIKE $1::varchar; SELECT t.id FROM t WHERE t.s NOT ILIKE $1",
            "SELECT t.id FROM t WHERE t.s SIMILAR TO $1::text; SELECT t.id FROM t WHERE t.s SIMILAR TO $1",
            "SELECT t.id FROM t WHERE $1::text LIKE 'a%'; SELECT t.id FROM t WHERE $1 LIKE 'a%'",
            // The subject of a `BETWEEN` and of an `IN` list.
            "SELECT t.id FROM t WHERE $1::int BETWEEN 1 AND 10; SELECT t.id FROM t WHERE $1 BETWEEN 1 AND 10",
            "SELECT t.id FROM t WHERE $1::date BETWEEN t.lo::date AND t.hi::date; SELECT t.id FROM t WHERE $1 BETWEEN t.lo::date AND t.hi::date",
            "SELECT t.id FROM t WHERE $1::int IN (1, 2, 3); SELECT t.id FROM t WHERE $1 IN (1, 2, 3)",
            "SELECT t.id FROM t WHERE $1::text IN ('a', 'b'); SELECT t.id FROM t WHERE $1 IN ('a', 'b')",
            "SELECT t.id FROM t WHERE $1::uuid NOT IN (t.u::uuid, t.v::uuid); SELECT t.id FROM t WHERE $1 NOT IN (t.u::uuid, t.v::uuid)",
            // A count.
            "SELECT t.id FROM t LIMIT $1::bigint OFFSET $2::int; SELECT t.id FROM t LIMIT $1 OFFSET $2",
            "SELECT t.id FROM t WHERE t.a IN (SELECT u.a FROM u LIMIT $1::int); SELECT t.id FROM t WHERE t.a IN (SELECT u.a FROM u LIMIT $1)",
            // `IS NULL` beside any of them.
            "SELECT t.id FROM t WHERE ($1::int IS NULL OR t.id > $1::int); SELECT t.id FROM t WHERE ($1 IS NULL OR t.id > $1::int)",
            // Two bare parameters are compared as `text`, and are one type between them.
            "SELECT t.id FROM t WHERE t.s = $1::text AND $1 = $2; SELECT t.id FROM t WHERE t.s = $1 AND $1 = $2",
            "SELECT t.id FROM t WHERE $1::text <> $2::text; SELECT t.id FROM t WHERE $1 <> $2",
            "SELECT t.id FROM t WHERE $1::text IS DISTINCT FROM $2; SELECT t.id FROM t WHERE $1 IS DISTINCT FROM $2",
            // Two rows are compared field by field.
            "SELECT t.id FROM t WHERE (t.u, t.a) = ($1::uuid, $2::bigint); SELECT t.id FROM t WHERE (t.u, t.a) = ($1, $2)",
            "SELECT t.id FROM t WHERE ($1::text, 5) < ('a', $2::int); SELECT t.id FROM t WHERE ($1, 5) < ('a', $2)",
            // `date(x)` is a `date`, whatever `x` is.
            "SELECT t.id FROM t WHERE DATE(t.ts) = $1::date; SELECT t.id FROM t WHERE DATE(t.ts) = $1",
            "SELECT t.id FROM t WHERE $1::date >= pg_catalog.date(t.ts); SELECT t.id FROM t WHERE $1 >= pg_catalog.date(t.ts)",
        ] {
            // The cast over the parameter is gone, so the two sides are one query.
            let (q, _) = run(sql).expect(sql);
            assert_eq!(q[0], q[1], "{sql}");
        }
    }
}
