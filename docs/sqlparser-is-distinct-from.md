# DRAFT — not filed, and fixed upstream in 0.63.0

A bug report for `apache/datafusion-sqlparser-rs`, written up but **never filed with that project**.
Publishing it here is disclosure, not a submission: it has had no upstream review, and it may be
wrong about that project's intent.

sqlparser 0.63.0 parses the examples below as expected, so there is nothing left to file. It is
kept as the record of why `src/normalize.rs` guards these shapes: that guard now refuses the 0.62
tree rather than repairing it, so a release that brought the bug back would be refused, not trusted.

---

## `IS [NOT] DISTINCT FROM` parses its right operand at precedence 0

**Version:** 0.62.0 (fixed in 0.63.0)

### What happens

The right operand of `IS [NOT] DISTINCT FROM` swallows every following operator, including `AND`
and `OR`:

```rust
use sqlparser::{dialect::GenericDialect, parser::Parser};

let sql = "SELECT * FROM t WHERE a IS DISTINCT FROM 1 AND b = 2";
println!("{:#?}", Parser::parse_sql(&GenericDialect {}, sql).unwrap());
```

The `WHERE` clause comes out as

```text
IsDistinctFrom(
    Identifier("a"),
    BinaryOp { left: Number("1"), op: And, right: BinaryOp { left: b, op: Eq, right: 2 } },
)
```

i.e. `a IS DISTINCT FROM (1 AND b = 2)`.

### What is expected

```text
BinaryOp {
    left:  IsDistinctFrom(Identifier("a"), Number("1")),
    op:    And,
    right: BinaryOp { left: b, op: Eq, right: 2 },
}
```

i.e. `(a IS DISTINCT FROM 1) AND (b = 2)`. PostgreSQL's [operator precedence table][pg] puts the
`IS` family above `NOT`, `AND` and `OR`, so `AND` cannot be part of the right operand.

A discriminating expression, well-typed under both readings:

```sql
SELECT true IS DISTINCT FROM true AND false;
-- (true IS DISTINCT FROM true) AND false  =  false AND false  =  false   <- correct
--  true IS DISTINCT FROM (true AND false) =  true IS DISTINCT FROM false =  true    <- as parsed here
```

DuckDB, which follows PostgreSQL precedence here, returns `false`.

`IS NOT DISTINCT FROM` behaves identically. Everything adjacent is correct: `IS NULL`, `IS TRUE`,
`BETWEEN`, `IN`, `LIKE`, `SIMILAR TO` and prefix `NOT` all produce the `AND` at the top.

### Cause

`src/parser/mod.rs`, in the `Keyword::IS` arm of `parse_infix`:

```rust
} else if self.parse_keywords(&[Keyword::DISTINCT, Keyword::FROM]) {
    let expr2 = self.parse_expr()?;                      // <-- precedence 0
    Ok(Expr::IsDistinctFrom(Box::new(expr), Box::new(expr2)))
} else if self.parse_keywords(&[Keyword::NOT, Keyword::DISTINCT, Keyword::FROM]) {
    let expr2 = self.parse_expr()?;                      // <-- precedence 0
    Ok(Expr::IsNotDistinctFrom(Box::new(expr), Box::new(expr2)))
```

`parse_expr` is `parse_subexpr(0)`. Every other infix branch in the same function uses
`self.parse_subexpr(precedence)` with the precedence `parse_infix` was called at — for example the
generic binary-operator branch a few lines above. These two are the exception.

### Suggested fix

Use the same precedence the surrounding branches do:

```rust
let expr2 = self.parse_subexpr(precedence)?;
```

`precedence` here is `Self::BETWEEN_PREC`-adjacent in the `IS` arm's caller; whichever value the
neighbouring `IS NULL` / `IS TRUE` handling effectively binds at is the one that keeps the whole
`IS` family at one precedence level.

Suggested regression tests (under 0.62, every one but the parenthesized control produces an
`IsDistinctFrom` at the root by swallowing what follows it):

```text
a IS DISTINCT FROM 1 AND b = 2       ->  (a IS DISTINCT FROM 1) AND (b = 2)
a IS NOT DISTINCT FROM 1 OR b = 2    ->  (a IS NOT DISTINCT FROM 1) OR (b = 2)
a IS DISTINCT FROM 1 AND b OR c      ->  ((a IS DISTINCT FROM 1) AND b) OR c
a IS DISTINCT FROM 1 OR b AND c      ->  (a IS DISTINCT FROM 1) OR (b AND c)
a IS DISTINCT FROM (1 AND b)         ->  unchanged (explicit parens)
a IS DISTINCT FROM b IS NULL         ->  not IsDistinctFrom(a, IsNull(b))
```

The last one is a separate symptom of the same precedence-0 call: the right operand swallows the
trailing `IS NULL`. What it should parse to is a judgement call rather than a fix, because
PostgreSQL declares the `IS` family non-associative (`%nonassoc IS` in its grammar), so the
expression is a syntax error there. This frontend refuses it whichever way it is nested.

### Why it matters to us

We lower SQL to an SMT-backed equivalence prover. The prover is sound given faithful IR, so a
mis-parse is not a wrong answer we can shrug at — it hands the prover a *different predicate* than
the query states, and two inequivalent queries can then lower to two equivalent formulas. The
shape occurs in real rewrite pairs, so this was not a hypothetical risk for us.

[pg]: https://www.postgresql.org/docs/current/sql-syntax-lexical.html#SQL-PRECEDENCE
