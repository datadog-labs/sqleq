# How `sqleq` is validated

A tool that answers "are these two queries equivalent?" is only worth as much as the reason to
believe its answers. This page is that reason: the method, what it can and cannot establish, and the
real defects it has caught — in this project's own code and in the solvers it drives.

It is deliberately free of benchmark numbers. Coverage is measured against corpora that are not
distributed with this repository, so any figure would be a record of one run on one machine rather
than a result a reader could reproduce. What is below does not depend on one.

## The shape of the problem

Query equivalence is undecidable in general, so no tool decides every pair. What a tool can do is be
*sound in one direction*:

| | can establish | can never establish | a non-answer means |
|---|---|---|---|
| a **prover** | equivalent | non-equivalent | nothing about the pair |
| a **disprover** | non-equivalent | equivalent | nothing about the pair |

`sqleq` runs both kinds. The proving axes are the [QED](https://github.com/qed-solver/prover)
prover and `sqleq-solver`, a Rust rewrite of [SQLSolver](https://github.com/SJTU-IPADS/SQLSolver),
both reading the IR the frontend produces, and the Lean axis, which proves one class of `INSERT`
pair under a parameter rule of its own ([LEAN.md](LEAN.md)). The refuting axis is `sqleq-fuzz`,
which runs the original SQL on PostgreSQL on generated instances and reports the first divergence.
It reads no IR, so it checks the frontend's lowering as well as the provers.

The single most important consequence: **"not proved" is not "not equivalent."** It is a statement
about reach, not about the queries. Most pairs in any realistic corpus come back undecided, and that
is the expected outcome rather than a failure.

## Proofs and refutations control each other

The provers and the refuter are not attempts at the same measurement. Each is sound in one
direction only, so none can check itself — but together they can, in exactly two places. Every pair
is classified by each independently, and the two kinds of answer are cross-tabulated:

|  | refuted | not refuted |
|---|---|---|
| **proved** | **soundness alarm** — one of them is wrong | expected |
| **not proved** | expected | undecided; the work queue |

The `proved × refuted` cell must be empty. Anything in it is a bug in a prover, in the disprover, or
in the frontend that feeds them, and every occurrence has been chased to a root cause — several are
listed below.

A proof and a counterexample contradict each other only when both answer the same question, which
for a parameterized pair means the same parameter binding. A Lean proof holds under the gather rule
and a `sqleq-fuzz` counterexample under index binding, so that combination is a non-equivalence,
not an alarm. `sqleq-check --portfolio` runs every asked backend on each case, whether the cases are
pair files or the rows of a corpus (`--corpus`), and reports the first cell as the verdict `alarm` — which fails the run
whatever `--expect` says — and the second as `not-equivalent`.

The `not proved × refuted` cell is what keeps the rest honest. It is the disprover demonstrating, on
this very run, that it is awake and can still refute things. If it ever collapsed toward zero, every
"no counterexample" in the run would be vacuous and the empty soundness cell would mean nothing.

**A run is therefore its own control, and no separate control batch is used or should be added.**
Earlier work paired each claim with a hand-built negative control; in a run that asks both a
prover and the refuter, that control set *is* a cell of the table, so it is reported as a
first-class number instead.

The [pinned pairs](../tests/pairs/README.md) are not such a batch. They record what each axis said
about pairs whose truth is already known — most of them defects found once, below — and fail when
any answer moves. That catches a regression on every axis, including the ones that did not find
the defect, but it cannot stand in for the cross-tab: a pinned pair only fails in a way someone has
already seen.

## Refuse, never guess

The frontend's hard rule: **if a construct cannot be lowered faithfully, emit nothing and say why.**
Never approximate, never fall through to a more general path that happens to typecheck.

This rule was bought with a bug. An early version silently mis-lowered a `GROUP BY` query: the
aggregate path did not match, so the query fell through to plain projection, which dropped the
grouping and treated `COUNT(...)` as an ordinary scalar function. That particular case merely failed
to prove — safe, as it happens — but the pattern is the one that produces a false proof, and it only
takes one shape where the dropped detail is load-bearing.

So refusals are a feature and are reported as such, in four kinds: `parse`, `unsupported`, `schema`,
and `parameter-misaligned`. A refusal costs coverage, which is visible and fixable. A wrong answer
costs trust, which is not.

The cost is real and paid knowingly: guards that refuse a shape rather than assume a property have
each given up provable pairs. That trade is accepted every time.

## Defects this method has caught

These are the durable findings — each one checkable in the code today, and each found by one axis
disagreeing with another rather than by inspection. Those that can be stated as a pair are pinned,
minimized, under [`tests/pairs/`](../tests/pairs/README.md).

**In the QED prover** (external, upstream). A scalar aggregate over an empty input returns one row;
the prover's grouping model did not, making it unsound on that shape. Found by `sqleq-fuzz`,
root-caused, and fixed with an empty-keys guard in the prover itself, which is not part of this
repository. Whether a given prover build carries the fix is checkable: one without it proves
`tests/pairs/aggregates/scalar_agg_empty_group.sql`, and the pinned run fails there.

**In the SQL parser we depend on.** `sqlparser`'s `IS DISTINCT FROM` parsed its right operand at
precedence 0, so the operand swallowed every conjunct that followed it: `a IS DISTINCT FROM b AND c`
became `a IS DISTINCT FROM (b AND c)`. A predicate that quietly means something else is precisely
how a *sound* prover is made to emit a false proof, and a second instance of the same shape turned
up later in JSON extraction. The first is fixed in `sqlparser` 0.63, which this repository uses, and
the frontend still refuses the shapes the mis-parse produced rather than trust the fix
(`normalize::fix_precedence`). The second is avoided by parsing with the Postgres dialect, which
gives the JSON operators their Postgres precedence. Both are pinned by tests that assert the
precedence rather than the output.

**In our own normalizations.** `strip_in_exists_distinct` removed `DISTINCT` inside `IN`/`EXISTS`
unconditionally. That is sound only in the absence of `LIMIT`/`OFFSET`: de-duplication changes which
rows a limit keeps. The guard is per-subquery, and deliberately uniform over `IN` and `EXISTS` even
though bare `LIMIT` under `EXISTS` is in fact harmless — a carve-out for half of one case is not
worth a second rule.

**In the assumption that `$1` on one side is `$1` on the other.** Two queries can use the same
parameter names for different things, and a rewrite can renumber parameters across a pair. Treating
the names as aligned is a false-proof channel; it is now detected and refused as
`parameter-misaligned`. The tempting shortcuts here are refuted by real pairs rather than argued
away — see [SOUNDNESS.md](SOUNDNESS.md), which sets out what the frontend still assumes.

**In the disprover, four times.** A disprover's failure mode is the mirror image: a *false
refutation*, claiming non-equivalence that does not hold. Each of these was caught and closed.
Comparing a `WITH`-wrapped DML statement against table state compared the wrong observable.
DuckDB binds `->` and `->>` looser than `AND`/`OR`/`NOT`, so a JSON predicate regrouped itself.
Schema construction invented a phantom `UNIQUE` column, lost a qualified table name, mishandled an
unknown cast type, and failed to absorb a table. And a declared `text[]` column was materialized as
a scalar `VARCHAR`.

**In what the disprover printed.** Rows violating `UNIQUE` or `NOT NULL` are dropped at insert, so
the instance actually tested is a subset of the rows generated — but the counterexample was rendered
from the generated set, printing witnesses that contained duplicate keys under a `UNIQUE` the schema
itself declared. The verdicts were never affected; the evidence offered for them was wrong, which is
its own kind of defect.

## What is not claimed

* **No completeness claim.** Undecided is the common outcome. The undecided set is a work queue, not
  a set of hard pairs.
* **"No counterexample found" is not a proof.** It is a finite search over small random instances,
  and it degrades with the generator's reach — a predicate comparing against a literal outside the
  generated value domain is satisfied by no row, and the pair looks equivalent when it may not be.
  See [`sqleq-fuzz/README.md`](../sqleq-fuzz/README.md).
* **A counterexample *is* a proof of non-equivalence**, provided the instance is valid and both
  queries are deterministic. Enforcing exactly that is most of what the disprover's code does.
* **Numbers expire.** Coverage figures move with every frontend change and are tied to whichever
  corpus produced them, so a figure is only ever a statement about one run.

## Reading further

* [SOUNDNESS.md](SOUNDNESS.md) — what a verdict rests on, and what the frontend still assumes.
* [DESIGN.md](DESIGN.md) — why the frontend is built the way it is.
