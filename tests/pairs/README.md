# Pinned pairs

Each `.sql` file here is a pair whose truth is known — equivalent or not in Postgres — together with
what every axis said about it when it was last reviewed. `tools/sqleq_check.py --expect pinned`
asks the axes again and fails on any movement. Most of these pairs are a defect that was found
once, kept as the smallest pair that shows it, so that from then on it is checked on every axis and
not only on the one that found it: a false proof found in one prover is a pin on all of them.

They are a **regression pin, not a control.** A pinned pair can only fail in a way someone has
already seen, so passing them says nothing about whether a change that grows the provable set is
sound. That evidence is still the cross-check in [CONTRIBUTING.md](../../CONTRIBUTING.md) rule 2.

`examples/dropped_filter.sql` and `examples/in_vs_or.sql` carry the same header and run with these.
The Lean axis's pairs are under `insert_unnest/`, and are stated under the gather rule
([below](#the-gather-rule)).

## Running them

```sh
# The axes CI runs, one job each:
python3 tools/sqleq_check.py --expect pinned --axes frontend,fuzz,sqlsolver-rust tests/pairs examples/*.sql
# The two it never installs (one SQLSolver per run):
python3 tools/sqleq_check.py --expect pinned --axes qed --prover "$QED_PROVER" tests/pairs examples/*.sql
python3 tools/sqleq_check.py --expect pinned --axes sqlsolver-jvm tests/pairs examples/*.sql
# The Lean axis, which needs `lake` on PATH; CI checks its pins with `cargo test -p sqleq-lean`:
python3 tools/sqleq_check.py --expect pinned --axes lean tests/pairs examples/*.sql
```

The output is one row per pair and one column per axis that ran. Exit code 0 means every pin held,
1 that something moved or broke a rule below, 2 that a tool is missing or a flag is wrong. Add
`--bless` to rewrite the `expect` lines from what the axes said, then read the diff.

## A pinned pair

```sql
-- <licence header>

-- truth: not-equivalent
-- expect frontend: emit
-- expect fuzz: counterexample
-- expect qed: no-proof
-- expect sqlsolver-rust: no-proof
-- expect sqlsolver-jvm: proved !known-unsound
-- catalog: inferred-seeded
-- origin: a boolean in the SELECT list was read two-valued, as if NULL were false
-- witness: t = {(1, NULL)}, $1 = 1: A yields false, B yields NULL

create table "t" ("id" INTEGER, "x" INTEGER, unique ("id"));
SELECT "x" IS NOT NULL AND "x" <= 5 FROM "t" WHERE "id" = $1;
SELECT "x" <= 5 FROM "t" WHERE "id" = $1;
```

Directives are read from the leading comment block only, up to the first SQL line, and only in the
form `-- key: value` with a lowercase key. Any other comment is prose. A directive below the SQL is
an error, not a comment, because a pin nobody reads looks exactly like one that holds.

| directive | written by | |
|---|---|---|
| `truth:` | a person; `--bless` never writes it | `equivalent` or `not-equivalent` |
| `expect <axis>:` | `--bless` | what that axis said, as one word from the table below |
| `binding:` | a person | `index` (the default) or `gather`: how the truth binds `$N` across the two sides ([below](#the-gather-rule)) |
| `catalog:` | a person | `declared` (the default), `inferred` or `inferred-seeded`. A pair that uses `$N` needs an inferred catalog: under the declared one the frontend refuses a bare placeholder. |
| `origin:` | a person; required | why the pair is here — the defect, and the commit or PR that fixed it |
| `witness:` | a person | for a non-equivalent pair, an instance on which the two sides differ. Required unless the pair pins `expect fuzz: counterexample` (not under `binding: gather`). |
| `argument:` | a person | for an equivalent pair, why it is one. Required unless a prover's pin is `proved` or `proved-literal`, or, under `binding: gather`, Lean's is `proved-gather`. |

## What each axis may say

Only the stable kind of an answer is pinned, never its message, so rewording a refusal moves
nothing and changing what is refused moves a pin.

| axis | words |
|---|---|
| `frontend` | `emit`, `emit-reflexive` (the two sides lowered to the same IR), `refuse:parse`, `refuse:unsupported`, `refuse:schema`, `refuse:parameter-misaligned` |
| `fuzz` | `counterexample`, `no-counterexample`, `param-misaligned`, `not-comparable`, `nondet-skip`, `no-schema`, `no-tables`, `error` |
| `qed` | `proved`, `proved-literal` (proved, from the same IR on both sides), `no-proof`, `no-plan` (the frontend refused), `panic`, `error` |
| `sqlsolver-rust`, `sqlsolver-jvm` | `proved`, `proved-literal`, `no-proof`, `unsupported` (the bridge could not express the plan), `no-plan`, `error` |
| `lean` | `proved-gather`, `no-witness` (proved, but possibly vacuously), `unsupported`, `invalid-sql`, `error` — see [LEAN.md](../../docs/LEAN.md) |

`timeout` and `missing` are never pinnable: they say the run got no answer, not what the answer was.

Read two of these with care. A prover's `no-proof` is not a refutation. A `no-counterexample` is
not a proof. `sqleq-fuzz` draws values from a small domain (see its
[README](../../sqleq-fuzz/README.md)), so a non-equivalent pair whose witness needs a varchar
length, a date at infinity or a second session is pinned `no-counterexample` and carries a
`witness:` instead.

## What fails

| what the run sees | result |
|---|---|
| an answer that contradicts `truth`: a prover (Lean included) proves a non-equivalent pair, the frontend lowers one to the same IR on both sides, or `sqleq-fuzz` refutes an equivalent one | **fails; `--bless` will not pin it** |
| the same, on a line marked `!known-unsound` | passes while the bug reproduces |
| a `!known-unsound` line whose answer no longer contradicts `truth` | fails; `--bless` drops the marker |
| a pinned answer that moved, either way | fails; `--bless` takes the new answer |
| an axis that ran with no `expect` line | fails; `--bless` adds one |
| `timeout` or `missing` | fails; shrink the pair or raise `--timeout` |
| a header error: no `truth` or `origin`, an unknown key, axis or word, a duplicate, a directive below the SQL, a pin that contradicts `truth` without a marker, a marker on one that contradicts nothing, or no evidence for the truth | fails; `--bless` skips the file |
| a line for an axis this run did not ask | not checked |

An improvement fails too. That is deliberate: the review of the diff is the point, and a proof
that appeared for no reason the author can give is the shape of a soundness bug.

A pin that contradicts `truth` is a header error even when its axis does not run, so a hand-edited
pin is caught in every CI run, not only where that axis is installed.

## The gather rule

Every axis but Lean binds `$N` by number: `$1` on one side is `$1` on the other. The Lean axis
proves `INSERT … VALUES` against `INSERT … SELECT * FROM unnest(…)` under the gather rule instead:
the `unnest` side's array `$j` is column `j` of the `VALUES` rows ([LEAN.md](../../docs/LEAN.md)).
A pair headed `-- binding: gather` states its truth under that rule, and its `witness:` or
`argument:` binds the parameters the same way.

Only an axis answering under a pair's binding can contradict its truth or stand as evidence for it.
Under `binding: gather` that is Lean alone; the other axes refuse to compare a scalar with an array
at the same `$N`, and their pins record exactly that. Under the default binding, Lean's answers are
ordinary pins.

## `!known-unsound`

A soundness bug that is not fixed yet is pinned by a person, never by `--bless`: write the line
with the contradicting answer and the marker, and say in the pair's comment which component is
wrong. The run then passes while the bug reproduces, and fails the run that fixes it, until
`--bless` drops the marker. A marker can therefore never outlive its bug unnoticed. One marker
covers one axis's line, and a marker on an answer that contradicts nothing is a header error.

## Adding a pair

1. **Minimize it**, on an invented schema. A pair found on a corpus that is not public is rewritten
   until nothing in it comes from there: table and column names, literals, shape.
2. **Write `truth`, `origin`, and a `witness` or an `argument`.** The truth is a claim about
   Postgres, not about any axis, and the witness or argument is what lets a reviewer check it.
3. **Rebuild every binary, then bless with every axis you have.** `--bless` refuses a binary in this
   repository's `target/` that is older than its sources, because the pins would record an older
   tree's answers. Bless the axes in as many runs as you need; each touches only its own lines.
4. **Read the diff against `truth`.** A `‼` in the run is a live soundness bug: fix it, or mark the
   line `!known-unsound` and open an issue.
5. **Show that it fails without the fix**: run the pair against a build from before the fix, and
   see the `‼` or `✗` it is there to catch.
6. **In the pull request, say why every pin that moved, moved.**

## Last review on every axis

The `qed` and `sqlsolver-jvm` pins are checked only where those tools are installed, so they can
drift between reviews. The last time all five axes were blessed together:

* **qed** — [qed-solver/prover](https://github.com/qed-solver/prover) with its empty-grouping-set
  fix applied, on Z3 5.1.0 and cvc5 1.4.1. A build without that fix proves
  `aggregates/scalar_agg_empty_group.sql`, and the run fails there, as it should.
* **sqlsolver-jvm** — the unpublished fork described in [SQLSOLVER.md](../../docs/SQLSOLVER.md),
  at its revision `8c5548b`.
* **sqlsolver-rust**, **frontend**, **fuzz**, **lean** — this tree; the solver on Z3 5.1.0 and Lean on
  the toolchain `lean/lean-toolchain` names, as in CI.

The truths of the `binding: gather` pairs were checked on Postgres 16: each side run as a prepared
statement, the `unnest` side under the gather binding of the `VALUES` side's parameters, once on an
empty table and once twice over.
