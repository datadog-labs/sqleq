// Unless explicitly stated otherwise all files in this repository are licensed under the
// Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/).
// Copyright 2026-Present Datadog, Inc.

//! A metamorphic test of name resolution: pairs whose two sides resolve one name differently, which
//! the frontend has to keep apart.
//!
//! A resolution rule is a soundness argument. When the frontend resolves a name the way Postgres
//! does not, two different queries become one tree, and no prover can disagree with identical
//! input: the pair is proved from identical IR (`emit-reflexive`), or settled without a plan by the
//! reflexivity check (`reflexive`). The tests in `tests/soundness.rs` and `tests/reflexive.rs` pin
//! examples of such rules; this file pins the rules, by building the queries each rule tells apart.
//!
//! # Mutants
//!
//! Each [`Mutant`] is a seed query over a small catalog built to make names ambiguous ([`CATALOG`]:
//! two tables share their column names, one has a quoted mixed-case column) and a mutant of it, made
//! by one operator that changes what a name resolves to: qualifying a sort key that is also an
//! output name, swapping two aliases, a different schema per side, a quoted name against the folded
//! one, a comma against an explicit join, a `WITH` binding against its hand-inlined body, and so on.
//! Each mutant is non-equivalent by construction and carries a `witness`, an instance on which
//! Postgres returns different rows for the two sides; every witness was checked on Postgres 17, in
//! every context below.
//!
//! The property: in every context and under both catalog modes that read the DDL
//! (`CatalogSource::Declared` and `CatalogSource::InferredSeeded`, since several resolution bugs show
//! in only one of them), the pair
//!
//! * does not lower to identical IR (`lower_with(..)["queries"][0] != [1]`), and
//! * is not [`reflexive`].
//!
//! A refusal satisfies the first: refusing is always sound. So that the test cannot pass by refusing
//! everything, each [`Control`] is a mutant made by an operator that keeps the meaning (an alias
//! renamed consistently, `ORDER BY 1` against the name of column 1, `public.t` against `t`), and has
//! to keep lowering, most of them to identical IR.
//!
//! # Contexts
//!
//! Each pair is checked as written and inside each of [`CONTEXTS`]: as a derived table and as a
//! `WITH` binding, both read back with `SELECT *`. A context that returns its query's bag unchanged
//! keeps a non-equivalent pair non-equivalent and an equivalent one equivalent, and it changes which
//! normalizations apply (several run only at the top level), so one seed tests several paths.
//!
//! # Known open bugs
//!
//! Some mutants fail today: their bugs are known, and the fixes are separate changes that can land in
//! any order relative to this file. [`KNOWN_OPEN`] maps each such mutant to its issue. A mutant listed
//! there is tolerated whether it passes or fails; one that passes in every context and mode is
//! reported on stderr as passing (written past the test harness's capture, so it shows in a passing
//! run), and its entry should then be deleted. Every other failure fails the test, and so does an
//! entry that names no mutant. Once the fixes for every issue in the table have merged, the table
//! should be empty.

use std::io::Write as _;

use sqleq_frontend::{lower_with, reflexive, CatalogSource, FrontendError};

/// The seed catalog. `t` and `u` share `a` and `b`, so an unqualified or wrongly qualified name can
/// land on either; `m` has a quoted column `"A"`, which is not `a`; `w` is a third table to join.
const CATALOG: &str = r#"create table "t" ("id" INTEGER, "a" INTEGER, "b" INTEGER, "s" VARCHAR, unique ("id"));
create table "u" ("id" INTEGER, "a" INTEGER, "b" INTEGER, unique ("id"));
create table "w" ("id" INTEGER, "x" INTEGER);
create table "m" ("id" INTEGER, "A" INTEGER);"#;

/// One table `t`, declared without a schema, for the schema-qualifier operators.
const BARE_T: &str = r#"create table "t" ("id" INTEGER, "a" INTEGER);"#;

/// Two tables of one name in two schemas.
const TWO_SCHEMAS: &str = r#"create table "s1"."t" ("id" INTEGER, "a" INTEGER);
create table "s2"."t" ("id" INTEGER, "a" INTEGER);"#;

/// A table with a column named like the keyword `DEFAULT`.
const DEFAULT_COLUMN: &str = r#"create table "t" ("id" INTEGER, "a" INTEGER DEFAULT 0, "default" INTEGER);"#;

/// A semantics-changing mutant: `seed` and `mutant` are not equivalent in Postgres.
struct Mutant {
    /// `operator/what it does`, the key [`KNOWN_OPEN`] uses.
    name: &'static str,
    ddl: &'static str,
    seed: &'static str,
    mutant: &'static str,
    /// An instance on which the two sides differ in Postgres, and how.
    witness: &'static str,
    /// Whether the pair is a query that [`CONTEXTS`] can embed (a DML statement is not).
    nests: bool,
}

const fn mutant(name: &'static str, seed: &'static str, mutant: &'static str, witness: &'static str) -> Mutant {
    Mutant { name, ddl: CATALOG, seed, mutant, witness, nests: true }
}

/// A semantics-preserving mutant: `seed` and `mutant` are equivalent in Postgres.
struct Control {
    name: &'static str,
    ddl: &'static str,
    seed: &'static str,
    mutant: &'static str,
    /// What the frontend has to make of the pair, in every context and mode.
    expect: Expect,
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum Expect {
    /// Lower to identical IR, or be [`reflexive`].
    Identical,
    /// Lower, or be [`reflexive`]: the pair is not refused. For a control whose two sides the
    /// frontend lowers faithfully but not to one tree.
    Lowers,
}

const fn control(name: &'static str, seed: &'static str, mutant: &'static str, expect: Expect) -> Control {
    Control { name, ddl: CATALOG, seed, mutant, expect }
}

/// Embeds a query in a larger one.
type Wrap = fn(&str) -> String;

/// Wrappers that return their query's bag unchanged: equal bags stay equal, different ones stay
/// different.
const CONTEXTS: &[(&str, Wrap)] = &[
    ("top level", |q| q.to_owned()),
    ("derived table", |q| format!("SELECT * FROM ({q}) AS ctx")),
    ("WITH binding", |q| format!("WITH ctx AS ({q}) SELECT * FROM ctx")),
];

const MODES: [CatalogSource; 2] = [CatalogSource::Declared, CatalogSource::InferredSeeded];

const MUTANTS: &[Mutant] = &[
    // --- A sort key is an output column only when it is a bare name (or a position). ---
    mutant(
        "order-by/qualify-a-key-named-like-an-output",
        "SELECT b AS a FROM t ORDER BY a LIMIT 1",
        "SELECT b AS a FROM t ORDER BY t.a LIMIT 1",
        "t = {(1, 1, 2, NULL), (2, 2, 1, NULL)}: A sorts by the output a (t.b) and returns 1, B by t.a and returns 2",
    ),
    mutant(
        "order-by/requalify-a-key-across-a-join",
        "SELECT u.a FROM t JOIN u ON t.id = u.id ORDER BY t.a LIMIT 1",
        "SELECT u.a FROM t JOIN u ON t.id = u.id ORDER BY u.a LIMIT 1",
        "t = {(1, 1, 0, NULL), (2, 2, 0, NULL)}; u = {(1, 5, 0), (2, 3, 0)}: A returns 5, B returns 3",
    ),
    mutant(
        "order-by/qualify-a-key-named-like-a-computed-output",
        "SELECT a + 0 AS b FROM t ORDER BY b LIMIT 1",
        "SELECT a + 0 AS b FROM t ORDER BY t.b LIMIT 1",
        "t = {(1, 1, 2, NULL), (2, 2, 1, NULL)}: A sorts by a + 0 and returns 1, B by t.b and returns 2",
    ),
    mutant(
        "order-by/an-expression-over-an-output-name",
        "SELECT b AS a FROM t ORDER BY a LIMIT 1",
        "SELECT b AS a FROM t ORDER BY a + 0 LIMIT 1",
        "t = {(1, 1, 2, NULL), (2, 2, 1, NULL)}: A sorts by the output a and returns 1; in an expression a is t.a, so B returns 2",
    ),
    mutant(
        "order-by/a-position-against-another-name",
        "SELECT b, a FROM t ORDER BY 1 LIMIT 1",
        "SELECT b, a FROM t ORDER BY a LIMIT 1",
        "t = {(1, 1, 2, NULL), (2, 2, 1, NULL)}: A sorts by b and returns (1, 2), B by a and returns (2, 1)",
    ),
    // --- Two aliases swapped, references unchanged. ---
    mutant(
        "from/swap-two-table-aliases",
        "SELECT x.a FROM t AS x JOIN u AS y ON x.id = y.id",
        "SELECT x.a FROM t AS y JOIN u AS x ON x.id = y.id",
        "t = {(1, 1, 0, NULL)}; u = {(1, 5, 0)}: A returns 1, B returns 5",
    ),
    mutant(
        "select/swap-two-output-aliases-under-order-by",
        "SELECT a AS b, b AS a FROM t ORDER BY a LIMIT 1",
        "SELECT a, b FROM t ORDER BY a LIMIT 1",
        "t = {(1, 1, 2, NULL), (2, 2, 1, NULL)}: A sorts by its second column and returns (2, 1), B by its first and returns (1, 2)",
    ),
    mutant(
        "select/swap-two-output-aliases-under-offset",
        "SELECT a AS b, b AS a FROM t ORDER BY a OFFSET 1",
        "SELECT a, b FROM t ORDER BY a OFFSET 1",
        "t = {(1, 1, 2, NULL), (2, 2, 1, NULL)}: A skips (2, 1) and returns (1, 2), B skips (1, 2) and returns (2, 1)",
    ),
    mutant(
        "select/swap-two-output-aliases-read-by-name",
        "SELECT d.a FROM (SELECT a AS b, b AS a FROM t) AS d",
        "SELECT d.a FROM (SELECT a, b FROM t) AS d",
        "t = {(1, 1, 2, NULL)}: A returns 2, B returns 1",
    ),
    // --- DISTINCT ON keys follow the ORDER BY rules. ---
    mutant(
        "distinct-on/swap-two-output-aliases",
        "SELECT DISTINCT ON (a) a AS b, b AS a FROM t",
        "SELECT DISTINCT ON (a) a, b FROM t",
        "t = {(1, 1, 0, NULL), (2, 2, 0, NULL)}: A's key is its output a (t.b), one row; B's is t.a, two rows",
    ),
    mutant(
        "distinct-on/qualify-a-key-named-like-an-output",
        "SELECT DISTINCT ON (a) a AS b, b AS a FROM t",
        "SELECT DISTINCT ON (t.a) a AS b, b AS a FROM t",
        "t = {(1, 1, 0, NULL), (2, 2, 0, NULL)}: A's key is its output a (t.b), one row; B's is t.a, two rows",
    ),
    mutant(
        "distinct-on/a-position-against-a-constant",
        "SELECT DISTINCT ON (1) a FROM t",
        "SELECT DISTINCT ON (1 + 0) a FROM t",
        "t = {(1, 1, 0, NULL), (2, 2, 0, NULL)}: A's key is column 1, two rows; B's is a constant, one row",
    ),
    mutant(
        "distinct-on/a-position-against-another-name",
        "SELECT DISTINCT ON (1) b, a FROM t",
        "SELECT DISTINCT ON (a) b, a FROM t",
        "t = {(1, 1, 0, NULL), (2, 2, 0, NULL)}: A's key is b, one row; B's is a, two rows",
    ),
    // --- A GROUP BY name is an input column first, unlike an ORDER BY name. ---
    mutant(
        "group-by/an-output-name-that-is-also-an-input-column",
        "SELECT b AS a, count(*) FROM t GROUP BY a, b",
        "SELECT b AS a, count(*) FROM t GROUP BY b",
        "t = {(1, 1, 5, NULL), (2, 2, 5, NULL)}: A groups by t.a and t.b, (5, 1) twice; B by t.b, (5, 2)",
    ),
    mutant(
        "group-by/a-position-against-more-keys",
        "SELECT a AS b, count(*) FROM t GROUP BY 1",
        "SELECT a AS b, count(*) FROM t GROUP BY a, b",
        "t = {(1, 1, 1, NULL), (2, 1, 2, NULL)}: A groups by a, (1, 2); B by a and b, (1, 1) twice",
    ),
    // --- An alias column list renames by position. ---
    mutant(
        "from/swap-a-derived-column-list",
        "SELECT d.a FROM (SELECT a, b FROM t) AS d(b, a)",
        "SELECT d.a FROM (SELECT a, b FROM t) AS d(a, b)",
        "t = {(1, 1, 2, NULL)}: A returns 2, B returns 1",
    ),
    mutant(
        "from/swap-a-table-column-list",
        "SELECT x.a FROM u AS x(id, b, a)",
        "SELECT x.a FROM u AS x(id, a, b)",
        "u = {(1, 1, 2)}: A returns 2, B returns 1",
    ),
    // --- A schema qualifier names a table. ---
    Mutant {
        ddl: BARE_T,
        ..mutant(
            "from/a-different-schema-per-side",
            "SELECT a FROM s1.t",
            "SELECT a FROM s2.t",
            "s1.t = {(1, 1)}; s2.t = {}: A returns 1, B no rows",
        )
    },
    Mutant {
        ddl: TWO_SCHEMAS,
        ..mutant(
            "from/a-different-schema-per-side-both-declared",
            "SELECT a FROM s1.t",
            "SELECT a FROM s2.t",
            "s1.t = {(1, 1)}; s2.t = {}: A returns 1, B no rows",
        )
    },
    // --- A quoted name keeps its case; an unquoted one folds to lower case. ---
    mutant(
        "select/a-quoted-column-against-the-folded-name",
        r#"SELECT "A" FROM m, t"#,
        "SELECT A FROM m, t",
        r#"m = {(1, 10)}; t = {(1, 1, 2, NULL)}: A reads m."A" and returns 10, B reads t.a and returns 1"#,
    ),
    mutant(
        "from/a-quoted-alias-against-the-folded-name",
        r#"SELECT "X".a FROM t AS "X", u AS x"#,
        r#"SELECT x.a FROM t AS "X", u AS x"#,
        "t = {(1, 1, 2, NULL)}; u = {(1, 5, 6)}: A reads t and returns 1, B reads u and returns 5",
    ),
    mutant(
        "order-by/a-quoted-output-name-against-the-folded-key",
        r#"SELECT b AS "A" FROM t ORDER BY A LIMIT 1"#,
        "SELECT b AS a FROM t ORDER BY A LIMIT 1",
        r#"t = {(1, 1, 2, NULL), (2, 2, 1, NULL)}: no output of A is named a, so A sorts by t.a and returns 2; B sorts by its output a and returns 1"#,
    ),
    mutant(
        "from/a-quoted-derived-column-against-the-folded-name",
        r#"SELECT s."B" FROM (SELECT a AS b, b AS "B" FROM t) AS s"#,
        r#"SELECT s.b FROM (SELECT a AS b, b AS "B" FROM t) AS s"#,
        "t = {(1, 1, 2, NULL)}: A returns 2, B returns 1",
    ),
    mutant(
        "with/a-quoted-binding-name-against-the-folded-name",
        r#"WITH "T" AS (SELECT a + 1 AS a FROM t) SELECT a FROM t"#,
        "WITH T AS (SELECT a + 1 AS a FROM t) SELECT a FROM t",
        r#"t = {(1, 1, 2, NULL)}: "T" is not t, so A reads the table and returns 1; B's binding shadows t and returns 2"#,
    ),
    Mutant {
        ddl: DEFAULT_COLUMN,
        nests: false,
        ..mutant(
            "update/the-keyword-default-against-a-column-named-default",
            "UPDATE t SET a = DEFAULT",
            r#"UPDATE t SET a = "default""#,
            "t = {(1, 5, 9)}: A leaves (1, 0, 9), B leaves (1, 9, 9)",
        )
    },
    // --- A comma binds looser than JOIN: `FROM a, b RIGHT JOIN c` is `a CROSS JOIN (b RIGHT JOIN c)`. ---
    mutant(
        "from/a-comma-before-a-right-join",
        "SELECT w.id FROM t, u RIGHT JOIN w ON u.a = w.x",
        "SELECT w.id FROM t CROSS JOIN u RIGHT JOIN w ON u.a = w.x",
        "t = {}; u = {(2, 1, 0)}; w = {(3, 1)}: A crosses the empty t and returns no rows, B keeps w's row and returns 3",
    ),
    mutant(
        "from/a-comma-before-a-full-join",
        "SELECT w.id FROM t, u FULL JOIN w ON u.a = w.x",
        "SELECT w.id FROM t CROSS JOIN u FULL JOIN w ON u.a = w.x",
        "t = {}; u = {(2, 1, 0)}; w = {(3, 1)}: A returns no rows, B returns 3",
    ),
    mutant(
        "from/a-comma-before-a-join-using",
        "SELECT t.id, u.id, w.id FROM t, u JOIN w USING (id)",
        "SELECT t.id, u.id, w.id FROM t JOIN w USING (id), u",
        "t = {(1, 0, 0, NULL)}; u = {(2, 0, 0)}; w = {(1, 0)}: A joins u to w and returns no rows, B joins t to w and returns (1, 2, 1)",
    ),
    mutant(
        "from/a-comma-item-is-not-in-scope-of-a-later-on",
        "SELECT o.id FROM w AS o WHERE EXISTS (SELECT 1 FROM w AS c, t JOIN u ON u.a = x)",
        "SELECT o.id FROM w AS o WHERE EXISTS (SELECT 1 FROM w AS c CROSS JOIN t JOIN u ON u.a = x)",
        "w = {(1, 7), (2, 5)}; t = {(10, 0, 0, NULL)}; u = {(20, 5, 0)}: A's ON cannot see c, so its x is the outer o.x and A returns 2; B's is c.x and B returns 1 and 2",
    ),
    // --- Inner scopes shadow outer ones. ---
    mutant(
        "subquery/an-unqualified-name-binds-the-innermost-scope",
        "SELECT id FROM t WHERE EXISTS (SELECT 1 FROM u WHERE u.b = a)",
        "SELECT id FROM t WHERE EXISTS (SELECT 1 FROM u WHERE u.b = t.a)",
        "t = {(1, 1, 0, NULL)}; u = {(1, 5, 1)}: A's a is u.a and returns no rows, B's is t.a and returns 1",
    ),
    mutant(
        "subquery/qualify-an-inner-name-as-the-outer-one",
        "SELECT id FROM t WHERE a IN (SELECT a FROM u)",
        "SELECT id FROM t WHERE a IN (SELECT t.a FROM u)",
        "t = {(1, 1, 0, NULL)}; u = {(1, 5, 0)}: A tests 1 IN {5} and returns no rows, B tests 1 IN {1} and returns 1",
    ),
    mutant(
        "with/a-binding-shadows-a-table",
        "WITH t AS (SELECT id, a + 1 AS a FROM u) SELECT a FROM t",
        "SELECT a FROM t",
        "t = {(1, 1, 0, NULL)}; u = {(1, 5, 0)}: A reads the binding and returns 6, B the table and returns 1",
    ),
    mutant(
        "with/a-recursive-binding-shadows-a-table",
        "WITH RECURSIVE t (a) AS (SELECT 1 UNION ALL SELECT a + 1 FROM t WHERE a < 3) SELECT a FROM t",
        "SELECT a FROM t",
        "t = {(1, 7, 0, NULL)}: A returns 1, 2 and 3, B returns 7",
    ),
    // --- A WITH binding is evaluated once; its inlined body, once per use. ---
    mutant(
        "with/a-volatile-binding-used-twice-against-its-inlined-body",
        "WITH c AS (SELECT random() AS r) SELECT x.r = y.r FROM c AS x, c AS y",
        "SELECT x.r = y.r FROM (SELECT random() AS r) AS x, (SELECT random() AS r) AS y",
        "any instance: A compares one value with itself and returns true, B two draws and returns false",
    ),
    mutant(
        "with/an-unlisted-volatile-binding-used-twice-against-its-inlined-body",
        "WITH c AS (SELECT random_normal() AS r) SELECT x.r = y.r FROM c AS x, c AS y",
        "SELECT x.r = y.r FROM (SELECT random_normal() AS r) AS x, (SELECT random_normal() AS r) AS y",
        "any instance: A returns true, B returns false",
    ),
    // --- A string literal that spells null is a string. ---
    mutant(
        "literal/the-string-NULL-against-null",
        "SELECT s FROM t WHERE s = 'NULL'",
        "SELECT s FROM t WHERE s = NULL",
        "t = {(1, 0, 0, 'NULL')}: A returns 'NULL', B no rows",
    ),
    mutant(
        "literal/the-string-null-against-null",
        "SELECT s FROM t WHERE s = 'null'",
        "SELECT s FROM t WHERE s = NULL",
        "t = {(1, 0, 0, 'null')}: A returns 'null', B no rows",
    ),
    mutant(
        "literal/the-string-null-is-not-null",
        "SELECT id FROM t WHERE 'null' IS NULL",
        "SELECT id FROM t WHERE NULL IS NULL",
        "t = {(1, 0, 0, NULL)}: A returns no rows, B returns 1",
    ),
];

/// Each mutant that fails today, and the issue that tracks it. See the module doc.
const KNOWN_OPEN: &[(&str, &str)] = &[
    ("order-by/qualify-a-key-named-like-an-output", "#46"),
    ("order-by/requalify-a-key-across-a-join", "#46"),
    ("order-by/qualify-a-key-named-like-a-computed-output", "#46"),
    ("distinct-on/swap-two-output-aliases", "#46"),
    ("distinct-on/qualify-a-key-named-like-an-output", "#46"),
    ("from/a-comma-before-a-right-join", "#47"),
    ("from/a-comma-before-a-full-join", "#47"),
    ("from/a-comma-item-is-not-in-scope-of-a-later-on", "#47"),
    ("select/a-quoted-column-against-the-folded-name", "#57"),
    ("from/a-quoted-alias-against-the-folded-name", "#57"),
    ("order-by/a-quoted-output-name-against-the-folded-key", "#57"),
];

const CONTROLS: &[Control] = &[
    control(
        "order-by/a-position-against-its-name",
        "SELECT b, a FROM t ORDER BY 1 LIMIT 1",
        "SELECT b, a FROM t ORDER BY b LIMIT 1",
        Expect::Identical,
    ),
    control(
        "order-by/an-output-name-against-its-position",
        "SELECT a AS b, b AS a FROM t ORDER BY a LIMIT 1",
        "SELECT a, b FROM t ORDER BY 2 LIMIT 1",
        Expect::Identical,
    ),
    control(
        "order-by/qualify-a-key-no-output-is-named",
        "SELECT a FROM t ORDER BY b LIMIT 1",
        "SELECT a FROM t ORDER BY t.b LIMIT 1",
        Expect::Identical,
    ),
    control(
        "order-by/a-quoted-lower-case-output-name",
        r#"SELECT b AS "a" FROM t ORDER BY A LIMIT 1"#,
        "SELECT b AS a FROM t ORDER BY a LIMIT 1",
        Expect::Identical,
    ),
    control(
        "set-operation/order-by-a-position-against-its-name",
        "SELECT a, b FROM t UNION ALL SELECT a, b FROM u ORDER BY 1 LIMIT 1",
        "SELECT a, b FROM t UNION ALL SELECT a, b FROM u ORDER BY a LIMIT 1",
        Expect::Identical,
    ),
    // `DISTINCT ON (1)` is lowered as a constant key today (#46), so the two do not meet yet.
    control(
        "distinct-on/a-position-against-its-name",
        "SELECT DISTINCT ON (1) a, b FROM t ORDER BY 1, b",
        "SELECT DISTINCT ON (a) a, b FROM t ORDER BY a, b",
        Expect::Lowers,
    ),
    control(
        "group-by/qualify-the-keys",
        "SELECT b AS a, count(*) FROM t GROUP BY a, b",
        "SELECT b AS a, count(*) FROM t GROUP BY t.a, t.b",
        Expect::Identical,
    ),
    control(
        "from/rename-an-alias-consistently",
        "SELECT x.a FROM t AS x WHERE x.b = 1",
        "SELECT y.a FROM t AS y WHERE y.b = 1",
        Expect::Identical,
    ),
    control(
        "from/swap-two-table-aliases-and-their-references",
        "SELECT x.a FROM t AS x JOIN u AS y ON x.id = y.id",
        "SELECT y.a FROM t AS y JOIN u AS x ON y.id = x.id",
        Expect::Identical,
    ),
    control(
        "from/rename-a-derived-column-consistently",
        "SELECT d.p FROM (SELECT a, b FROM t) AS d(p, q)",
        "SELECT d.r FROM (SELECT a, b FROM t) AS d(r, q)",
        Expect::Identical,
    ),
    // `public.t` against a bare `t` is refused, not stripped: which table a bare name reads depends on
    // the search path (docs/SOUNDNESS.md). One qualifier spelled two ways is still one table.
    Control {
        ddl: BARE_T,
        ..control("from/a-schema-against-its-folded-spelling", "SELECT a FROM public.t", "SELECT a FROM PUBLIC.t", Expect::Identical)
    },
    control(
        "select/a-quoted-lower-case-name-against-the-folded-name",
        r#"SELECT "a" FROM t"#,
        "SELECT A FROM t",
        Expect::Identical,
    ),
    control(
        "where/redundant-parentheses",
        "SELECT a FROM t WHERE (b = 1)",
        "SELECT a FROM t WHERE b = 1",
        Expect::Identical,
    ),
    control(
        "from/a-comma-against-a-cross-join",
        "SELECT t.id FROM t, u WHERE t.id = u.id",
        "SELECT t.id FROM t CROSS JOIN u WHERE t.id = u.id",
        Expect::Identical,
    ),
    // A LEFT join's left side is kept whole, so it commutes with the cross join; RIGHT and FULL do not.
    // The outputs are aliased apart: a derived table with two columns of one name is refused.
    control(
        "from/a-comma-before-a-left-join",
        "SELECT t.id AS tid, w.id AS wid FROM t, u LEFT JOIN w ON u.a = w.x",
        "SELECT t.id AS tid, w.id AS wid FROM (t CROSS JOIN u) LEFT JOIN w ON u.a = w.x",
        Expect::Lowers,
    ),
    control(
        "with/a-binding-against-its-inlined-body",
        "WITH c AS (SELECT a FROM t) SELECT c.a FROM c",
        "SELECT c.a FROM (SELECT a FROM t) AS c",
        Expect::Identical,
    ),
    control(
        "with/a-deterministic-binding-used-twice-against-its-inlined-body",
        "WITH c AS (SELECT a FROM t) SELECT x.a FROM c AS x, c AS y",
        "SELECT x.a FROM (SELECT a FROM t) AS x, (SELECT a FROM t) AS y",
        Expect::Identical,
    ),
    control(
        "subquery/qualify-an-outer-name-nothing-shadows",
        "SELECT id FROM t WHERE EXISTS (SELECT 1 FROM w WHERE w.x = a)",
        "SELECT id FROM t WHERE EXISTS (SELECT 1 FROM w WHERE w.x = t.a)",
        Expect::Identical,
    ),
];

/// What the frontend made of one pair in one catalog mode.
enum Lowered {
    Identical,
    Different,
    Refused(FrontendError),
}

fn lower(src: &str, mode: CatalogSource) -> Lowered {
    match lower_with(src, mode) {
        Ok(v) if v["queries"][0] == v["queries"][1] => Lowered::Identical,
        Ok(_) => Lowered::Different,
        Err(e) => Lowered::Refused(e),
    }
}

/// Every input one case expands to: `(context, src)`.
fn inputs(ddl: &str, seed: &str, mutant: &str, nests: bool) -> Vec<(&'static str, String)> {
    let contexts = if nests { CONTEXTS } else { &CONTEXTS[..1] };
    contexts.iter().map(|(name, wrap)| (*name, format!("{ddl}\n{};\n{};", wrap(seed), wrap(mutant)))).collect()
}

/// Write past the test harness's output capture, so a note shows in a passing run too.
fn note(line: &str) {
    let _ = writeln!(std::io::stderr(), "{line}");
}

#[test]
fn semantics_changing_mutants_stay_apart() {
    let mut unexpected = Vec::new();
    for m in MUTANTS {
        let mut failures = Vec::new();
        for (context, src) in inputs(m.ddl, m.seed, m.mutant, m.nests) {
            if reflexive(&src) {
                failures.push(format!("{context}: reflexive"));
            }
            for mode in MODES {
                match lower(&src, mode) {
                    Lowered::Identical => failures.push(format!("{context}, {mode:?}: identical IR")),
                    // A pair the parser cannot read tests nothing: the case is what is wrong.
                    Lowered::Refused(FrontendError::Parse(e)) => {
                        failures.push(format!("{context}, {mode:?}: does not parse: {e}"))
                    }
                    Lowered::Different | Lowered::Refused(_) => {}
                }
            }
        }
        let known = KNOWN_OPEN.iter().find(|(k, _)| *k == m.name).map(|(_, issue)| issue);
        match (known, failures.is_empty()) {
            (None, true) => {}
            (None, false) => {
                unexpected.push(format!("{}: {}\n    witness: {}", m.name, failures.join("; "), m.witness))
            }
            (Some(issue), true) => note(&format!("now passes, delete its KNOWN_OPEN entry ({issue}): {}", m.name)),
            (Some(issue), false) => eprintln!("known open ({issue}): {}: {}", m.name, failures.join("; ")),
        }
    }
    assert!(unexpected.is_empty(), "non-equivalent pairs the frontend made one query:\n  {}", unexpected.join("\n  "));
}

#[test]
fn semantics_preserving_mutants_still_lower() {
    let mut failures = Vec::new();
    for c in CONTROLS {
        for (context, src) in inputs(c.ddl, c.seed, c.mutant, true) {
            let reflexive = reflexive(&src);
            for mode in MODES {
                let got = lower(&src, mode);
                let ok = reflexive
                    || match got {
                        Lowered::Identical => true,
                        Lowered::Different => c.expect == Expect::Lowers,
                        Lowered::Refused(_) => false,
                    };
                if !ok {
                    let got = match got {
                        Lowered::Identical => "identical IR".to_owned(),
                        Lowered::Different => "different IR".to_owned(),
                        Lowered::Refused(e) => format!("refused: {e}"),
                    };
                    failures.push(format!("{} [{context}, {mode:?}]: expected {:?}, got {got}", c.name, c.expect));
                }
            }
        }
    }
    assert!(failures.is_empty(), "equivalent pairs the frontend no longer handles:\n  {}", failures.join("\n  "));
}

#[test]
fn each_known_open_entry_names_a_mutant() {
    for (name, issue) in KNOWN_OPEN {
        assert!(MUTANTS.iter().any(|m| m.name == *name), "KNOWN_OPEN names no mutant: {name} ({issue})");
    }
    let mut names: Vec<&str> = MUTANTS.iter().map(|m| m.name).chain(CONTROLS.iter().map(|c| c.name)).collect();
    names.sort_unstable();
    let before = names.len();
    names.dedup();
    assert_eq!(before, names.len(), "two cases share a name");
}
