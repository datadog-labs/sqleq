# The Lean axis (`sqleq-lean`)

`sqleq-lean` proves one class of pair that the other axes cannot even state. It writes the proof in
Lean 4 and has the Lean kernel check it. It proves and never refutes: a pair it cannot prove is left
undecided. The class is an `INSERT … VALUES` against an `INSERT … SELECT * FROM unnest(…)`:

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
compared as row *sequences*, never as bags, because three things depend on row order: the serial
ids the rows get, which of two duplicates `DO NOTHING` keeps, and the order `RETURNING` reports.

### Generated cells

A `VALUES` cell can be one the database fills in: `DEFAULT`, `nextval('s')`, or a generator such as
`now()` or `gen_random_uuid()`. Often the `unnest` side supplies that column's array itself:

```sql
INSERT INTO t (id, name) VALUES (DEFAULT, $1), (DEFAULT, $2);
INSERT INTO t (id, name) SELECT * FROM unnest($1::int[], $2::text[]);
```

There is no parameter to gather for such a cell, so the pair is proved under a weaker claim with
its own verdict, `proved-gather-generated`. The theorem (`EquivGatherGen`) adds one more
quantifier: for every value `g i j` that the generated cell in row `i`, column `j` could evaluate
to, the `unnest` side, run under the gather binding with those cells' entries taken from `g`, gives
the same result as the `VALUES` side whose generated cells evaluate to `g`.

So the `unnest` side reproduces the `VALUES` side **when given the values the `VALUES` side's
generated cells produced**. Nothing in the claim says where it would get them. For a serial or an
identity column, it means the caller supplies the ids, which leaves the sequence behind them, so a
later `DEFAULT` can collide with an id the `unnest` side inserted. As a rewrite that is not safe,
though the claim holds. A record whose generated cells draw from a sequence says so
(`generated.sequence`).

## What a proof assumes

Only this: an `INSERT`'s effect is determined by its target, its column list, its tail, and the
sequence of typed rows its source yields. For `proved-gather-generated`, one more: a generated cell
contributes only its value. What a generator does besides produce its value (a sequence it
advances) is not compared, and the claim speaks of runs in which every generated cell evaluates
without error.

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
  - A generated cell is `DEFAULT`, `nextval('s')`, or one of a short list of generators called with
    no argument: the clocks (`now()`, `current_timestamp`, `localtimestamp`, `clock_timestamp()`,
    …) and the uuid generators. A generator with a parameter, a cast over one, and any other
    function are refused.
- **Parameter placement.** Each `VALUES` parameter is pinned to one column: `$n` sits in column
  `(n − 1) mod k`. A parameter spread over columns of different types fails Postgres's type
  inference, and the model cannot see that failure. Nor may a parameter number go unused, which
  would leave it with no type at all. With generated cells, which take a column but no number,
  the parameters must instead be `$1, $2, …` in reading order, each once.
- **The tail.** The two tails are identical and mention no parameter, because a parameter number
  means different things on the two sides: on one side `$k+1` may be a `VALUES` cell, while on the
  other the same `$k+1` is the `DO UPDATE` value.
- **The `unnest` arguments.** They are `$1..$k` in order, one per column, with no `WITH
  ORDINALITY`, `WHERE`, `ORDER BY`, `LIMIT` or `DISTINCT`.

A pair with generated cells is also refused when its schema says a generated cell is not just its
value:
- a generated cell in a `GENERATED ALWAYS` column, which accepts `DEFAULT` but rejects the `unnest`
  side's explicit value;
- a target table whose DDL was not read in full (a statement naming it could not be read, it is
  `ALTER`ed, or it only parsed after the frontend's simplifying retry), since such a column, or a
  rule that evaluates the generators again, could be missing;
- a generated cell drawing from a sequence while something else in the statement reads that
  sequence's state: the tail (`nextval`, `currval`, `DEFAULT` in `DO UPDATE SET`), an omitted
  column's default, or a trigger. The `VALUES` side advances the sequence row by row and the
  `unnest` side does not;
- `DEFAULT` on a column whose default is not a constant, a sequence or a recognised generator;
- a generator whose value the column's type cannot take, or two generators in one column.

Every condition above that concerns the two statements is checked twice: once in Rust, to give a
refusal its reason, and once by the kernel, from each side's own text. The ones that need the
schema (the list just above), and the unused-parameter check, which is about what Postgres will
prepare, are checked in Rust only, because the kernel never sees the schema.

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
  unrecognised), or a fresh value per row (a sequence, an identity, `gen_random_uuid()`),
  including a domain's default for a column of that domain;
- generated cells: `DEFAULT` filled as an omitted column would be, a per-row generator as a fresh
  value, and any other generator as the column's once-per-statement value, which maximises
  collisions;
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
- a table whose DDL only parsed after the frontend's simplifying retry, which can drop a `NOT NULL`;
- a table with a column of a domain whose DDL could not be read (sqlparser has no domain-level
  `NOT NULL`), since such a statement constrains the column but never names the table.

pg_dump's `CREATE UNIQUE INDEX … ON ONLY t` is read as the same index without `ONLY`.

The model is used only for non-vacuity, so an error in it cannot make a proof false. It can only
credit or withhold credit wrongly.

## Verdicts

| verdict | meaning |
|---|---|
| `proved-gather` | The kernel proved `EquivGather` and checked a witness. The credited verdict. |
| `no-witness` | The kernel proved `EquivGather`, but the `VALUES` side fails every run with non-NULL parameters, or its table's DDL could not be read reliably enough to tell. The proof may be vacuous, so it is not credited as evidence, though it is still a claim. |
| `proved-gather-generated` | The kernel proved `EquivGatherGen`, the weaker claim for generated cells, and checked a witness. Credited under its own name, for a pair whose truth is stated in that weaker form. |
| `no-witness-generated` | The kernel proved `EquivGatherGen`, but no witness shows the `VALUES` side can succeed. A claim, not credited, as `no-witness` is. |
| `unsupported` | Outside the fragment above. Not a claim about the pair. |
| `invalid-sql` | Postgres rejects the pair as written, e.g. a `VALUES` row narrower than the column list. |
| `error`, `timeout` | Lean did not accept the proof, or did not finish. |

How `sqleq-check` reads them follows from what each claims (`sqleq-check/src/suite.rs`). In the
pinned suite a pair headed `-- binding: gather` is credited by `proved-gather` alone: a generated
proof claims less than that truth. Under `-- binding: gather-generated` either proof counts, since a
plain gather proof claims more. A proof the binding counts is a claim of equivalence whether or not
a witness was found, so a `no-witness` pinned against a not-equivalent truth fails the run as a
false proof would. Under `--portfolio` a Lean proof gives a verdict of its own rather than plain
`equivalent`: `proved-gather` makes a case `equivalent-gather`, and `proved-gather-generated` makes
it `equivalent-gather-generated`. A Lean proof and a `sqleq-fuzz` counterexample answer under
different bindings, so together they make a case `not-equivalent`, not an alarm.

A proof counts only if both of these checks pass:

- **What was proved.** Before any proof in a generated Lean file is believed, the file must pass an
  audit (`run::audit`), a second definition of the file format written apart from the emitter:
  - every line must be one the emitter writes;
  - each proof must state exactly `EquivGather A B`, or `EquivGatherGen A B`, of its own pair's `A`
    and `B`. The verdict follows which, so a pair is `proved-gather-generated` because the kernel
    checked that statement, and the kernel checks that its `VALUES` side has a generated cell;
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

`tools/lean_replay.py` is an independent check that uses no Lean. Its input is a replay plan:
`sqleq-check --axes lean --expect report-only --lean-replay-plan plan.json` over pair files or a
`--corpus` has `sqleq-lean` record, for each pair the kernel proved (`proved-gather`, `no-witness`,
or either one's `-generated` form), its DDL, both statements, the `VALUES` rows, and the target's
column types and defaults. `--portfolio` does not pass the option on. The script then replays each
pair on a real Postgres:

- the `VALUES` side under the canonical binding, and the `unnest` side under the gather binding
  built from the same values;
- once on an empty table, and once run twice, so the second run meets existing rows through the
  conflict clause.

The two sides must agree on outcome, row count, `RETURNING` rows in order, and table contents. The
witness model must agree with Postgres on whether the `VALUES` side succeeds.

Every run is a transaction that applies the DDL and is rolled back. Inside it, generators are made
deterministic so that both runs see the *same* stream, which is what the theorem claims. A clock
is fixed, and a random value is drawn from a sequence created within the run: one per default in
the DDL and per generator in a tail, and one shared by the generators in the `VALUES` clause, so a
draw on one side only shifts nothing on the other. Schema qualifiers are dropped, which is how
sqleq resolves a name.

For a pair with generated cells, a probe first runs the `VALUES` clause into a copy of the table
that has the target's defaults and identities but no constraints, and reads back what the generated
cells evaluated to. Postgres does the `DEFAULT` substitution, the coercion and the evaluation order
itself. The `unnest` side is then run with those values gathered into its arrays, a fresh set for
each execution. A difference between the sides that the probe explains (the `VALUES` side inserted
other generated values than it found) is reported as inconclusive rather than as an alarm.

    python3 tools/lean_replay.py --setup --plan plan.json --json replay.json --host <socket dir>

`ALARM-sides-differ` is the outcome the axis must never produce. `witness-disagrees` is a gap in
the witness model, and the direction matters: a credited pair that fails in Postgres means credit
was wrong, while a withheld one that succeeds only cost credit.

## Running it

It needs a Lean toolchain (`elan`, which installs the version in `lean/lean-toolchain`), and runs
through `sqleq-check` like every other axis:

```sh
cargo build --release && cargo build --release -p sqleq-lean
./target/release/sqleq-check --axes lean --expect report-only tests/pairs/insert_unnest
./target/release/sqleq-check --axes lean --expect report-only --corpus corpus.csv --json out.json
```

`sqleq-check` finds `sqleq-lean` in this repository's `target/` unless `--lean-bin`, `--bin-dir` or
`$SQLEQ_LEAN` names one, and `--lean` adds the axis to any other run, `--portfolio` included.
`$LAKE` overrides the `lake` found on `PATH`, and `$SQLEQ_LEAN_DIR` the Lean package. The checker's
own positive and negative controls are `lake build SqleqTest` in `lean/`.

`sqleq-lean` can also be run on its own, over pair files, directories of them, or `--csv
<corpus.csv>`, writing its own per-pair record (`sqleq-lean --help` lists the options). A few
options have no `sqleq-check` counterpart, among them `--batch` (pairs per Lean file) and
`--translate-only`, which runs no Lean and reports, for each pair, its refusal or the claim it would
be checked under.

The axis's pinned pairs are in [`tests/pairs/insert_unnest/`](../tests/pairs/insert_unnest/),
headed `-- binding: gather` because their truth is stated under the gather rule, or `-- binding:
gather-generated` for a pair with generated cells; [the pinned-pair README](../tests/pairs/README.md)
describes the format. `cargo test -p sqleq-lean` checks every `-- expect lean:` pin under
`tests/pairs/` against real Lean, and `sqleq-check --expect pinned --axes lean tests/pairs` does the
same with the suite's other rules.
