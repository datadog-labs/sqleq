# The Lean axis (`sqleq-lean`)

`sqleq-lean` decides one class of pair that the other axes cannot even state. It proves the pair
in Lean 4 and has the Lean kernel check the proof. The class is an `INSERT … VALUES` against an
`INSERT … SELECT * FROM unnest(…)`:

```sql
INSERT INTO t (a, b) VALUES ($1, $2), ($3, $4);
INSERT INTO t (a, b) SELECT * FROM unnest($1::int[], $2::text[]);
```

On the `VALUES` side `$1` is one value; on the `unnest` side it is an array. So the usual rule, that
`$N` means the same value on both sides, gives this pair no meaning.

## What a proof claims

**The gather rule.** The `unnest` side's array `$j` is column `j` of the `VALUES` rows, in row
order. For the pair above, `$1 = [$1, $3]` and `$2 = [$2, $4]`, written in terms of the `VALUES`
side's parameters. A pair is proved *under the gather rule*, so its verdict is never plain `proved`.

The theorem (`EquivGather` in `lean/Sqleq/Check.lean`) says the following. For every value type,
every binding of the `VALUES` side's parameters, and every function `run` from (target, columns,
tail, row sequence) to a result, the `unnest` side run under the gather binding gives the same
result as the `VALUES` side. The *tail* is everything after the source: the table alias, the
conflict clause and `RETURNING`.

`run` is universally quantified, so the proof does not depend on how `ON CONFLICT`, defaults, NOT
NULL, unique, CHECK and foreign-key constraints, sequences, clocks or triggers behave. Sources are
compared as row *sequences*, never as bags, because serial ids, which duplicate `DO NOTHING` keeps,
and `RETURNING` order all depend on row order.

## What a proof assumes

Only this: an `INSERT`'s effect is determined by its target, its column list, its tail, and the
sequence of typed rows its source yields.

The checker enforces the conditions that make "typed rows" the same on both sides, and refuses
everything else:

- **Types.** Each `unnest` element type is exactly its column's declared base type, with aliases
  resolved. A `VALUES` cell is `$n`, `$n` cast to its column's base type, or `NULL`.
  - A modifier on the *column* (`varchar(10)`, `numeric(10,2)`, `timestamp(3)`) is fine. Both sides
    reach it through the same assignment coercion from the base type: the same length check that
    raises, and the same rounding.
  - A modifier on a *cast* is refused, because an explicit cast to `varchar(10)` truncates where
    assignment raises.
  - Array-typed columns are refused, because `unnest` flattens every dimension. `T ARRAY` counts
    as an array type, as `T[]` does, and an `unnest` argument spelled `$j::T ARRAY` is refused, as
    the frontend refuses that spelling.
  - With a mismatched type the two sides apply different coercions. `text[]` into a `uuid` column
    always errors, and `int4[]` into `bigint` differs on overflow.
- **Parameter placement.** Each `VALUES` parameter is pinned to one column: `$n` sits in column
  `(n − 1) mod k`. A parameter spread over columns of different types fails Postgres's type
  inference, and the model cannot see that failure.
- **The tail.** The two tails are identical and mention no parameter. On one side `$k+1` may be a
  `VALUES` cell, while on the other the same `$k+1` is the `DO UPDATE` value.
- **The `unnest` arguments.** They are `$1..$k` in order, one per column, with no `WITH
  ORDINALITY`, `WHERE`, `ORDER BY`, `LIMIT` or `DISTINCT`.

Every condition is checked twice: once in Rust, to give a refusal its reason, and once by the kernel,
from each side's own text.

## Non-vacuity

A pair whose `VALUES` side always errors satisfies `EquivGather` trivially, because both sides fail
the same way every time. Two identical rows under a unique key do this, and so does an omitted NOT
NULL column with no default. So a proof is only credited together with a **witness**, checked by
the kernel (`Sqleq.Witness`). The witness is one run of the `VALUES` side, on an empty table, that
succeeds and inserts at least one row.

The run gives every parameter its own non-NULL value. That is the strongest choice: with distinct
parameters, two rows collide only where the statement itself forces equal values (the same
parameter, or the same once-per-statement default). So if this run fails, every run with non-NULL
parameters fails.

The model covers:
- defaults: none (so NULL), one value per statement (a constant, a clock, or anything
  unrecognised), or a fresh value per row (a sequence, an identity, `gen_random_uuid()`);
- NOT NULL;
- unique constraints and unique indexes, including `NULLS NOT DISTINCT`;
- conflict-target inference, `DO NOTHING`, and `DO UPDATE`, which cannot touch a row the same
  statement inserted;
- `GENERATED ALWAYS` columns.

CHECK and foreign-key constraints are not modelled; a record carries `unmodelled` when the target
table has either (or a trigger).

A missed constraint is the one direction in which the model would invent a witness, so the
following tables get none:
- a table with an `EXCLUDE` constraint;
- a table the DDL also `ALTER`s, since constraints added that way are not read;
- a table named by any DDL statement that could not be read;
- a table whose DDL only parsed after the frontend's simplifying retry, which can drop a `NOT NULL`.

pg_dump's `CREATE UNIQUE INDEX … ON ONLY t` is read as the same index without `ONLY`.

The model is used only for non-vacuity, so an error in it cannot make a proof false. It can only
credit or withhold credit wrongly.

## Verdicts

| verdict | meaning |
|---|---|
| `proved-gather` | The kernel proved `EquivGather` and checked a witness. The credited verdict. |
| `no-witness` | The kernel proved `EquivGather`, but the `VALUES` side fails every run with non-NULL parameters, or its table's DDL could not be read reliably enough to tell. The proof may be vacuous. Not credited. |
| `unsupported` | Outside the fragment above. Not a claim about the pair. |
| `invalid-sql` | Postgres rejects the pair as written, e.g. a `VALUES` row narrower than the column list. |
| `error`, `timeout` | Lean did not accept the proof, or did not finish. |

A proof counts only if both of these checks pass:

- **What was proved.** Before any proof in a generated Lean file is believed, the file must pass an
  audit (`run::audit`), a second definition of the file format written apart from the emitter:
  - every line must be one the emitter writes;
  - each proof must state exactly `EquivGather A B` of its own pair's `A` and `B`;
  - every definition must be pure data, built only from the package's constructors, numerals and
    booleans;
  - nothing else may appear, so no `axiom`, no `set_option` (such as `debug.skipKernelTC`), and no
    macro or tactic that could change how the file is checked.
- **How it was proved.** `#print axioms` on the proof lists nothing beyond `propext`,
  `Classical.choice` and `Quot.sound`. A proof that fell back to `sorry` shows `sorryAx`, and
  `native_decide` shows `Lean.ofReduceBool`; both are rejected.

Lean's own kernel is still trusted. Re-checking the proofs with an independent kernel (for example
`nanoda` through `lean4export`, as Lean's `comparator` does) is a natural next step.

## Checking it against Postgres

`tools/lean_replay.py` is an independent check that uses no Lean. `sqleq-lean --replay-plan
plan.json` records, for each proved or no-witness pair, its DDL, both statements, the `VALUES`
rows, and the target's column types and defaults. The script then replays each pair on a real
Postgres:

- the `VALUES` side under the canonical binding, and the `unnest` side under the gather binding
  built from the same values;
- once on an empty table, and once run twice, so the second run meets existing rows through the
  conflict clause.

The two sides must agree on outcome, row count, `RETURNING` rows in order, and table contents. The
witness model must agree with Postgres on whether the `VALUES` side succeeds.

Every run is a transaction that applies the DDL and is rolled back. Inside it, generators are made
deterministic so that both runs see the *same* stream, which is what the theorem claims. A clock
default is fixed, and a random uuid is drawn from a sequence created within the run. Schema
qualifiers are dropped, which is how sqleq resolves a name.

    python3 tools/lean_replay.py --setup --plan plan.json --json replay.json --host <socket dir>

`ALARM-sides-differ` is the outcome the axis must never produce. `witness-disagrees` is a gap in
the witness model, and the direction matters: a credited pair that fails in Postgres means credit
was wrong, while a withheld one that succeeds only cost credit.

## Running it

It needs a Lean toolchain (`elan`, which installs the version in `lean/lean-toolchain`):

```sh
cargo build -p sqleq-lean
./target/debug/sqleq-lean tests/pairs/insert_unnest # pair files, or directories of them
./target/debug/sqleq-lean --csv corpus.csv --json out.json
```

`$LAKE` overrides the `lake` found on `PATH`, and `$SQLEQ_LEAN_DIR` the Lean package. The checker's
own positive and negative controls are `lake build SqleqTest` in `lean/`.

The axis's pinned pairs are in [`tests/pairs/insert_unnest/`](../tests/pairs/README.md), headed
`-- binding: gather` because their truth is stated under the gather rule. `cargo test -p
sqleq-lean` checks every `-- expect lean:` pin under `tests/pairs/` against real Lean, and
`tools/sqleq_check.py --expect pinned --axes lean` does the same with the suite's other rules.
