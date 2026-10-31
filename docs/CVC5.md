# The cvc5 axis: `sqleq-cvc5`

`sqleq-cvc5` encodes a lowered pair as an SMT problem over [cvc5](https://cvc5.github.io/)'s theory
of bags of tuples -- or, as an approximation, of sets of tuples -- and asks cvc5 for a model. It is
the `cvc5` axis: `sqleq-check --cvc5` asks it about every case that lowered, over the very `Input`
JSON the QED prover and `sqleq-solver` read, and `--portfolio` asks it beside them. It follows the
Java translator of the LPAR'24 work on relations and tables in cvc5 (`Cvc5AbstractTranslator`,
`Cvc5BagsTranslator` and `Cvc5SetsTranslator` in the `spes` repository), operator for operator,
but reads this repository's IR instead of Calcite plans.

Two of its answers are claims, which is what sets it apart from the SQLSolver axes. `unsat` is a
proof of equivalence under the encoding: `proved`. `sat` comes with a model, a database on which
cvc5's encodings of the two queries differ: `model`. The second refutes **the plan**, not the pair:
it is a database under the encoding's readings of the types ([below](#what-the-encoding-approximates))
and under the plan's constraints alone, and the frontend drops the constraints it cannot carry (an
expression or opclass unique index, `UNIQUE NULLS NOT DISTINCT`, a foreign key), so the database may
be one the DDL forbids. cvc5 checks it against its own assertions (`check-models`), but it is not
an executed counterexample as `sqleq-fuzz`'s is, and the harness keeps the two words apart: a
`model` contradicts a proof over the same plan -- that is an alarm, and the cross-check this axis
adds -- and decides no portfolio verdict on its own. It is claimed only from an **exact** problem,
one with no uninterpreted symbol in it; a model found under an uninterpreted function also fixes
that function's values, which the real one may not take, and is reported as a candidate under
`no-proof` ([below](#uninterpreted-symbols)).

## Building it

The crate links a cvc5 library through the [cvc5-rs](https://github.com/cvc5/cvc5-rs) bindings
(`cvc5 = "0.6"`, which needs cvc5 1.4.0 or later) and reads the IR through `sqleq-solver`, so its
first build also compiles Z3. The library is the one `CVC5_LIB_DIR` names, with its headers at
`CVC5_INCLUDE_DIR` or `$CVC5_LIB_DIR/../include`, and `build.rs` puts that directory in the
binary's rpath. The bindings need `libclang` to generate their FFI.

The axis is written against the `bags-algebra` branch of cvc5
([mudathirmahgoub/cvc5#27](https://github.com/mudathirmahgoub/cvc5/pull/27)), whose bag reasoning it
exercises: normal forms for the relational terms the encoding builds, the keys and `NOT NULL`
columns of the tables read off their constraints (a preprocessing pass, `bags-facts`), and lemmas
for folds, maps and disequalities. No cvc5 release has it yet, so one script fetches that branch at
a pinned commit, builds it, installs it under `target/cvc5/install`, and builds the crate against
it:

```sh
sh tools/build_cvc5.sh            # once; then, like the other crates:
cargo build --release -p sqleq-cvc5
cargo test -p sqleq-cvc5
```

The first run compiles cvc5 for 5 to 15 minutes, by the cores, and needs git, cmake 3.16 or later,
a C++17 compiler and Python 3; cvc5 downloads and builds CaDiCaL, SymFPU, LibPoly, MPFR and, where
the system has none, GMP, and installs the shared ones beside itself.
A second run with the same commit only rebuilds the crate. `.cargo/config.toml` points
`CVC5_LIB_DIR` at the install, so cargo needs no environment; to build against a cvc5 of your own,
set `CVC5_LIB_DIR=<prefix>/lib` (the headers are read from `$CVC5_LIB_DIR/../include` or
`$CVC5_INCLUDE_DIR`), or `SQLEQ_CVC5_REPO` and `SQLEQ_CVC5_COMMIT` to have the script build another
commit. Any cvc5 from 1.4.0 on runs the encoding, but proves far fewer pairs without the branch.
The crate is opt-in like `sqleq-fuzz` and `sqleq-solver` (not a default workspace member), and CI
does not build it.

## Running it

```sh
# The cvc5 axis beside the frontend, under bag semantics, 10s per case:
sqleq-check --axes frontend,cvc5 --expect report-only -t 10 -j 6 pairs/
# Set semantics, and a cvc5 option:
sqleq-check --cvc5 --cvc5-semantics sets --expect report-only pairs/
sqleq-check --cvc5 --cvc5-opt bags-deq-decompose=false --expect report-only pairs/
# In a portfolio, where a cvc5 proof against a fuzz counterexample, or a cvc5 model against a QED
# proof, is an alarm, and neither decides a verdict on its own:
sqleq-check --portfolio --axes qed,cvc5,fuzz --expect report-only -v pairs/
```

| flag | what |
|---|---|
| `--cvc5` | Ask the axis; the same as adding `cvc5` to `--axes`. A prover, so it brings the frontend in. |
| `--cvc5-bin PATH` | The binary (else `--bin-dir`, `$SQLEQ_CVC5_BIN`, `PATH`, this repo's build). |
| `--cvc5-semantics bags\|sets` | Bag semantics, SQL's own, is the default; set semantics collapses duplicates and is far easier for the solver. Under sets the answers are `proved-sets` and `model-sets`: about the approximation, so they neither claim nor refute anything about the pair. |
| `--cvc5-opt NAME=VALUE` | A further cvc5 `set-option`, repeatable. |
| `--cvc5-timeout MS` | cvc5's own time limit on each case's check (default `-t`), with the process killed at the case's deadline as the backstop. |

Outside `--portfolio`, the axis runs after the prover pass, one `sqleq-cvc5` process per case, `-j`
wide: the driver is cheap to start, and cvc5 does not always notice its time limit inside a long
rewrite, so a process the harness can kill is the only reliable cap. Under `--portfolio` it runs
beside the other backends within the case's deadline. Its answers never move the exit code of
`--expect equivalent` or `report-only`, except through an alarm; under `--expect pinned` its pins
are checked like any axis's.

**The axis is experimental.** Its bag reasoning lives in an unreleased branch of cvc5
([above](#building-it)), and its proofs are not checked. So a cvc5 `proved` is reported, and
against a sqleq-fuzz counterexample it is an alarm, like any prover's; but it decides no
`--portfolio` verdict (a pair only cvc5 proves is `undecided`) and is no evidence for a pinned
pair's truth (`sqleq-check/src/suite.rs`, `evidence_equivalent`), until the bag work is in a cvc5
release and the axis has been cross-checked against sqleq-fuzz. A `model` likewise decides nothing
alone: it refutes the plan, which lacks the constraints the frontend does not carry, not the pair.

The binary speaks `IrDriver`'s contract -- `sqleq-cvc5 <jobs.jsonl> <results.jsonl>
[--timeout-ms=N]` -- with `--semantics`, `--opt`, `--smt2=DIR` (each problem as SMT-LIB, for the
`cvc5` binary) and `--plan FILE` (one `Input` JSON, the row on stdout) besides; `sqleq-cvc5 --help`
is `src/main.rs`'s module doc.

### The buckets

| bucket | the driver said | meaning |
|---|---|---|
| `proved` | `EQ` | `unsat`: no database the encoding admits distinguishes the two queries. |
| `proved-literal` | `EQ`, `literal` | The two IR trees were identical (tier 0); never a proof. |
| `model` | `NEQ` with an exact model | `sat`: a database on which the two encodings differ, with no uninterpreted symbol in the problem. Refutes the plan (the suite's `ir` binding): an alarm against a proof over the same plan; no verdict on its own, and no contradiction with a pair pinned `truth: equivalent`, whose equivalence may rest on a constraint the plan lacks -- `tests/pairs/constraints/expression_unique_index.sql` is one, and is pinned `model`. |
| `no-proof` | `UNKNOWN`; `NEQ` under uninterpreted symbols; `NEQ` with no model | cvc5 gave up; or found a model that also interprets an unknown function, kept in the note as a candidate; or the two queries return rows of different sorts, which the IR's guard types can make of two queries that agree everywhere (`SUM` over integers is REAL to the frontend, a `CASE` over booleans and `NULL` INTEGER). |
| `unsupported` | `NOTRANS`, `NOIR` | The plan was not encoded: the reason, in `sqleq-solver`'s spelling where it is the IR reader's (`correlated`, `unsupported-sort`, `aggregate:ARRAY_AGG`), else `unsupported:<what>`. |
| `timeout` | `HANG`, or anything but a claim cut off at the cap | |
| `error` | `ERROR`, or the driver died | A cvc5 call failed, or cvc5 aborted: a model that failed its own check ends the process, which is recorded here rather than as a `model`. |

## The encoding

A table is a constant of sort `(Bag (Tuple (Nullable T1) ... (Nullable Tn)))` -- `(Set ...)` under
set semantics -- every column nullable whatever the schema says, because the encoding puts nulls
into intermediate results everywhere (an outer join pads with them, an aggregate over no rows is
one) and a column sort that tracked declared nullability would not match the terms flowing through
it. A `NOT NULL` column is instead a constraint, `(= (bag.count (tuple null) ((_ table.project i)
T)) 0)`, and a key one too: `(= (bag.setof ((_ table.project K) T)) ((_ table.project K) T))` says
the key's values form a set, so no two rows agree on it. The keys are the frontend's (`UNIQUE` over
`NOT NULL` columns only, see `docs/SQLSOLVER.md`); under set semantics no key is asserted, since a
set cannot repeat a row and the rest of the constraint would need cardinalities.

Each relational operator is one term over its inputs:

| IR | bags | sets |
|---|---|---|
| `filter` | `bag.filter (lambda t. cond)` | `set.filter` |
| `project` of column references | `(_ table.project i..)` | `(_ rel.project i..)` |
| `project` of expressions | `bag.map (lambda t. (tuple e..))` | `set.map` |
| `join` | `table.product` under a `bag.filter`; an outer join adds the unmatched rows of a side, a `bag.filter` keeping the rows no row of the other side joins with (an anti-join), mapped to a padded row | `rel.product`, `set.filter` |
| `distinct`, `intersect`, `except` | `bag.setof`, over `bag.inter_min` and `bag.difference_remove` | nothing, `set.inter`, `set.minus` |
| `intersectAll`, `exceptAll` | `bag.inter_min`, `bag.difference_subtract` | refused: multiplicities, which a set has not got |
| `union` (always `UNION ALL`) | `bag.union_disjoint` | `set.union` |
| `group` with no call | `bag.setof` of a projection | a projection |
| `group` with calls | `(_ table.aggr k..) f init T`, a fold whose accumulator holds the keys and one slot per call (two for `AVG`), with a hidden count of the group's rows when there are keys, since `table.aggr` folds the empty table as one empty group: its row is the one with count 0, filtered out before the counter is projected away | `rel.aggr` |
| `values` | `bag.union_disjoint` of `(bag row 1)` | `set.union` of `set.singleton` |
| `sort` | the source, when it has no `offset` or `limit`; `LIMIT 0` the empty bag; a row slice under an `ORDER BY` whose keys contain a key of the rows, so that no two rows tie, a `bag.filter` keeping the rows whose place -- a `bag.fold` counting the rows sorted before them -- lies in `[offset, offset + limit)`; refused under possible ties, for a computed size, and when `offset + limit` overflows an i64 | the source, or the empty set for `LIMIT 0`; a slice is refused |
| `EXISTS q` | `(distinct q bag.empty)` | |
| `l IN q`, one column | `(bag.member (tuple l) q)`, else null when `l` is null and `q` has a row or `q` holds a null, else false | |
| scalar subquery | a `bag.fold` keeping the value of the row it reads, seeded with null (the shape Calcite's `SINGLE_VALUE` folds to); encoded only when `q` has at most one row by shape -- an aggregate without `GROUP BY`, a `VALUES` of one row, `LIMIT 0` or `1`, and projections, filters, duplicate removal and bare sorts of those -- and refused otherwise, since Postgres evaluates an uncorrelated scalar subquery lazily, so a database on which `q` has two rows may still run the query, and a constraint `(<= (bag.card q) 1)` would prove pairs that differ on it | `(ite (= q set.empty) null ((_ tuple.select 0) (set.choose q)))`, under the same guard |

Expressions are lifted over the nullable columns with SQL's three-valued logic: a strict operator
is `(ite (or (is_null a) (is_null b)) null (some (op a b)))`, `AND`/`OR` test for their dominant
value across every operand before lifting (so `FALSE AND NULL` is `FALSE`), a `WHERE` or `CASE`
condition passes only on `some(true)`. `COUNT`, `SUM`, `MIN`, `MAX` skip nulls and, but for `COUNT`,
are null over no non-null value; `AVG` is `avg(sum, count)`, an uninterpreted function of the two,
as in `sqleq-solver`, since Postgres's `avg` over integers is a `numeric` with its own rounding;
`COUNT(DISTINCT x)` deduplicates its input first and can be the group's only call. Integer `/` and
`%` -- the operators, or the frontend's `q_arith_div_integer_integer` and `q_arith_mod_integer_integer`
-- truncate toward zero as Postgres does; `+`, `-`, `*` and their `q_arith_{add,sub,mul}_*` over
INTEGER and REAL are exact; `LIKE` against a literal pattern is a regular expression. Two sides are
compared as one bag term, `(not (= q1 q2))`; an integer column against a real one is widened to
reals first, as Postgres compares a `bigint` with a `numeric`.

Where this encoding departs from the Java translator, deliberately:

* **Casts and unknown functions are uninterpreted, never erased.** Java drops every `CAST`, which
  equates `CAST(a AS REAL) / b` with `a / b`. Here a cast is an uninterpreted function named by its
  source and target type, and so is every function the encoding does not interpret -- strict
  (null exactly when an argument is) where the frontend's naming guarantees it (`q_conv_*`,
  `q_arith_*` over other types, `q_cast_*`, `QCASTn`, and the handful `sqleq-solver` lists), and
  otherwise a function over the nullable sorts, which assumes nothing about null. `||`,
  `SUBSTRING`, `ABS` and `LIKE` against a literal pattern are interpreted only where the frontend
  typed them as cvc5 would; a call it types opaque is a function it does not know, and stays one
  here, named alike on both sides. `UPPER` and `LOWER` are never interpreted: cvc5's `str.to_upper`
  and `str.to_lower` map ASCII only, where Postgres maps by the collation's locale
  ([below](#what-the-encoding-approximates)).
* **`NOT NULL` is a constraint, not a sort**, as above. Java declares a table with the schema's
  sorts and maps it back to nullable columns, a `bag.map` at every scan.
* **Subqueries** are encoded, correlated ones included; Calcite rewrites them away before Java
  sees them. A column of an enclosing row read inside a subquery is the enclosing lambda's
  variable, which the subquery's own lambdas name apart from theirs. A scalar subquery is encoded
  only where its shape bounds it to one row (the table above).
* **Lambdas share their variables.** Every lambda over one sort binds the same variable, and
  nullable lifting is spelled out as an `ite` rather than cvc5's `nullable.lift`, which wraps the
  operator in a lambda with fresh variables each time. The bags solver relates two maps or filters
  by the identity of their functions and does not rewrite under the binder before comparing them,
  so two sides that compute `a + 5` the same way must be one term. No body is rewritten before it
  is bound: cvc5 rewrites the assertions, lambda bodies included, after its `bags-facts` pass has
  read the keys and `NOT NULL` columns off the constraints, and a body rewritten earlier would keep
  a form that pass has not seen; `a + 10 / 2` and `a + 5` meet there.

## What the encoding approximates

A proof holds, and a model is a database, under these readings of the IR's types:

* `INTEGER`, `DATE`, `TIME` and `TIMESTAMP` are cvc5 integers: unbounded, so no overflow and no
  `infinity`. A model may use a value Postgres has not got.
* `REAL` is a cvc5 real: an exact rational, so `2.0` and `2.00` are one value where Postgres prints
  them apart, and `1/3` is exact where `numeric` rounds. Numeric division is left to the frontend's
  uninterpreted `q_arith_div_real_*` for that reason.
* `VARCHAR` is a cvc5 string, so `=` is identity: a deterministic collation. Its order is code
  point order only where the frontend emitted a native comparison, which it does under the `C` and
  `POSIX` collations alone (`src/collation.rs`); `MIN`, `MAX` and a row slice ordered by a string
  read the database's default collation, which no input states, and fold with an uninterpreted
  strict order `str_lt:default` instead, as a comparison under any other collation is the
  frontend's uninterpreted `q_str_lt`. `UPPER` and `LOWER` are uninterpreted for the same reason:
  cvc5's `str.to_upper` maps ASCII, Postgres maps by the collation's locale. A character above
  U+2FFFF, the end of SMT-LIB's alphabet, is refused: cvc5 would keep its `\u{..}` escape as text,
  and the character and the text of its escape would be one string.
* The opaque `VARBINARY` and `INTERVAL` are uninterpreted sorts whose `=` is identity, which
  Postgres's is not (`'1 day' = '24 hours'`; `0 = -0` for `double precision`). A `VARBINARY`
  annotation on a value the encoding computed from typed operands -- a `COALESCE` over a REAL and
  an INTEGER, which the frontend cannot type -- keeps the computed sort.
* A `NULL` literal the frontend typed by default takes the sort it is wanted at.

These are the readings under which `proved` is a proof. They, and the constraints the plan lacks,
are why a `sat` is a `model` and not a `counterexample`: it is a database under them, and
`sqleq-fuzz`, which runs the pair on Postgres under its real DDL, remains the thing that confirms
one. Over the pinned pairs, three `model`s fall on pairs equivalent in Postgres: each is equivalent
through a unique index the frontend does not carry (`tests/pairs/constraints/`), and cvc5's
database violates that index.

## Uninterpreted symbols

An uninterpreted symbol cannot license a false proof: the real function is one of its
interpretations, so what holds under every interpretation holds under the real one. It can cost a
proof -- `CAST(x AS INTEGER) = x` is not provable when the cast is uninterpreted -- and it can admit
a model Postgres would not: cvc5 picks the function's values, and the model may rest on them. The
driver therefore reports whether the problem was exact (`model_exact`), and the harness claims a
`model` only then. A model under uninterpreted symbols is a candidate, kept in the `no-proof`
note as `candidate model, under uninterpreted symbols: ...`, for a person or for `sqleq-fuzz`.

## Known limits

* **The time limit is polled.** cvc5 honours `tlimit-per` where it spends resources, and a long
  rewrite or model construction can overrun it by seconds; the harness's process kill at the case's
  deadline is the real cap, and the standalone driver writes `HANG` and exits 3 after `--grace-ms`,
  as `sqleq-solver` does, for a caller to resume.
* **A model that fails `check-models` aborts cvc5**, and the driver with it (cvc5 raises an
  internal error through the C API). The harness records the case as an `error`; what it says is
  that cvc5 found a model its own checker rejects, which is the checker doing its job. Set
  semantics with `set.filter` lambdas is where it has been seen.
* **Refused**: a scalar subquery that may return more than one row (anything but an aggregate
  without `GROUP BY`, a `VALUES` of one row, a `LIMIT 0` or `1`, and projections, filters,
  duplicate removal or bare sorts of those), an aggregate other than
  `COUNT`/`SUM`/`AVG`/`MIN`/`MAX` and Calcite's `SINGLE_VALUE`, or one of several arguments but
  `COUNT`, `MIN`/`MAX` over a sort with no order cvc5 knows, a row slice under possible ties (no key
  of the rows among the `ORDER BY` keys), of a computed size, whose `OFFSET + LIMIT` overflows an
  i64, or under set semantics, `INTERSECT ALL` and `EXCEPT ALL` under set semantics, a string
  literal with a character above U+2FFFF, a set operation over branches of different row sorts,
  arithmetic or an order comparison over an opaque sort.
* **Bag semantics is hard.** Under bags cvc5 proves far less than under sets within a budget, and
  what it proves depends on the shape of the plan more than on the size of the pair: a projection
  that drops a column is a non-injective `bag.map`, which the bags solver reasons about weakly.
  Set semantics is the reading to try first when a pair is not about duplicates.
