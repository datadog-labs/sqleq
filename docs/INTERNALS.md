# Internals: where things live

Orientation for someone reading or changing the code. It is not needed to *use* `sqleq` — for that,
start at the [README](../README.md).

`sqleq-check` sits on top and runs every backend as a subprocess. `src/` is the frontend: it parses
a SQL pair, resolves names and types, and lowers it to the `Relation`/`Expr` IR the provers read —
the QED prover directly, and `sqleq-solver`, a Rust rewrite of SQLSolver, through the job files
`src/sqlsolver.rs` writes. The other backends read the pair themselves: `sqleq-fuzz` runs the two
statements against DuckDB looking for a counterexample and uses no part of the frontend, and
`sqleq-lean` parses with the frontend's parser but builds its own Lean terms. See
[DESIGN.md](DESIGN.md) for why the frontend refuses what it cannot lower.

## The frontend (`src/`)

| path | what |
|---|---|
| `src/lib.rs` | public API: `lower_sql`, and `lower_with`/`lower_with_ddl` over a `CatalogSource`; the reflexivity check (`reflexive`); the opt-in `internals` module that `sqleq-lean` and the benchmark harness link |
| `src/main.rs` | the `sqleq-frontend` CLI: one pair, a corpus CSV (`--csv`), or SQLSolver job files (`--sqlsolver`) |
| `src/{catalog,scope,types,casts,lower,error}.rs` | catalog, de-Bruijn scope, type mapping/coercion, cast rules, lowering, errors |
| `src/{normalize,dml}.rs` | equivalence-preserving rewrites; `DELETE`, `UPDATE` and `INSERT` pairs reduced to queries computing their effect |
| `src/{infer,pgddl}.rs` | type inference (the `--infer`/`--infer-seeded` modes) and the raw-Postgres-DDL reader |
| `src/params.rs` | `$N` handling and the misalignment check ([SOUNDNESS.md](SOUNDNESS.md)) |
| `src/verify.rs` | re-derives the de-Bruijn invariants on every lowering — a second net under the soundness argument |
| `src/corpus.rs` | the corpus CSV reader and `--csv` batch driver: row *n* is case `pair{n:04}`, lowered against its own DDL; `sqleq-check --corpus` reads rows through it |
| `src/sqlsolver.rs` | the job files both SQLSolver implementations read, `sqleq-solver` and the JVM fork ([SQLSOLVER.md](SQLSOLVER.md)) |

| test | what |
|---|---|
| `tests/lower.rs` | the public `lower_sql` API: parse/lower shape and the soundness refusals |
| `tests/soundness.rs` | lowerings that used to make two different queries look alike to a prover, each with a control |
| `tests/coverage.rs` | expressions lowered by naming what they compute: what must lower alike, what must not, what is still refused |
| `tests/temporal.rs` | DATE/TIME/TIMESTAMP/TIMESTAMPTZ/INTERVAL kept apart in the IR, and every crossing between them named |
| `tests/depth.rs` | long and deeply nested predicates stay within a prover's nesting limit |
| `tests/reflexive.rs` | the reflexivity check: that it reaches past a lowering refusal, and never widens one into a proof |
| `tests/doc_links.rs` | every relative link in every Markdown file resolves |
| `tests/pairs/` | pinned pairs: known truth, each axis's last answer, run by `sqleq-check --expect pinned` ([README](../tests/pairs/README.md)) |

Unit tests sit beside the code they test, in each module's `#[cfg(test)]` block — those for the
`DELETE`/`UPDATE`/`INSERT` reductions are in `src/dml.rs`.

## The command (`sqleq-check/`)

Pair files, a directory of them, or a corpus CSV in; per-axis answers, a report and a CI exit code
out ([manual](../sqleq-check/README.md)). It links the frontend's library, so a corpus row is
named and split exactly as every other tool reads it, but runs every backend as a subprocess, so
building it builds none of them.

| path | what |
|---|---|
| `sqleq-check/src/main.rs`, `cli.rs` | entry point and the command line (clap) |
| `sqleq-check/src/app.rs` | setup, the passes, `--jsonl` streaming and `--resume`, and the exit-code policy |
| `sqleq-check/src/discover.rs` | finding each backend binary, and noticing one built from older sources |
| `sqleq-check/src/inputs.rs` | which files are cases, what each is called, and whether a pair is `x` against `x` |
| `sqleq-check/src/case.rs` | one case through the frontend and the QED prover |
| `sqleq-check/src/axes/{fuzz,solver,lean}.rs` | the axes that run after the prover pass: `sqleq-fuzz`, the SQLSolver second opinion (`sqleq-solver` or the JVM fork), and `sqleq-lean` |
| `sqleq-check/src/portfolio.rs` | `--portfolio`: every backend at once on each case, under one deadline, and one combined verdict |
| `sqleq-check/src/suite.rs` | the pinned-pair header grammar, lint, judgement and `--bless` behind `--expect pinned` |
| `sqleq-check/src/pinned.rs` | each axis's answer reduced to the one word a pin records |
| `sqleq-check/src/report.rs` | the summary and per-axis tables it prints, and the `--json` and `--csv` files |
| `sqleq-check/src/proc.rs` | subprocesses in their own process groups, with a hard wall-clock timeout and Ctrl-C handling |
| `sqleq-check/src/util.rs` | temporary directories, `abspath`, `which`, an executable check |
| `sqleq-check/tests/through_main.rs` | the binary end to end over stand-in backends, including the controls a fake prover must fail |
| `sqleq-check/tests/{portfolio,corpus}.rs` | `--portfolio` and `--corpus` end to end, over stand-in backends |
| `sqleq-check/tests/real_cases.rs` | the hygiene gate over every committed pair: it lints, carries the licence, and names nothing from a corpus or a machine |
| `sqleq-check/tests/real_solver.rs` | the second opinion against the real `sqleq-solver`; skips without a build unless `$SQLEQ_SOLVER_BIN` is set |
| `tools/sqlsolver/` | our side of the IR bridge to the JVM SQLSolver, `sqleq-solver`'s backup cross-check: `IrToRel.java` builds the plan pair, `IrDriver.java` runs it; `sqleq-check --sqlsolver-jvm` compiles both ([SQLSOLVER.md](SQLSOLVER.md)) |

Running the provers and `sqleq-fuzz` over one corpus and cross-tabulating the answers is what
produces the table [VALIDATION.md](VALIDATION.md) describes, including the cell that is a soundness
alarm. `sqleq-check --portfolio` does that per case and reports the alarm as a verdict that fails
the run; `--corpus` points it, or any other run, at a corpus CSV instead of pair files.

## The SQLSolver rewrite (`sqleq-solver/`)

A Rust rewrite of SQLSolver's proof engine, opt-in because its first build compiles Z3 from source:
`cargo build -p sqleq-solver`. It reads the same `Input` JSON the QED prover gets and answers in the
JVM driver's row format, so the original SQLSolver, kept as its backup cross-check, can be swapped
in. What it rewrites, and where it deliberately differs from the original, is in
[SQLSOLVER.md](SQLSOLVER.md).

| path | what |
|---|---|
| `sqleq-solver/src/main.rs` | the binary, behind `IrDriver`'s command line and job/result JSONL |
| `sqleq-solver/src/{ir,translate,uterm}.rs` | the `Input` JSON as typed IR, and its translation into U-expressions |
| `sqleq-solver/src/{normalize,ic,alpha}.rs` | normalization, integrity-constraint rewrites, and the alpha-equivalence decision |
| `sqleq-solver/src/{prove,setsolver}.rs` | the decision ladder, and the Z3 set solver as its last rung |
| `sqleq-solver/src/eval.rs` | a concrete evaluator, so tests can check a rewrite against data |
| `sqleq-solver/examples/` | gates and diagnostics run over a job file: translation, the ladder, rung-3 statistics, normalization traces and checks |

## The refuting axis (`sqleq-fuzz/`)

A separate crate, opt-in because its first build downloads DuckDB: `cargo build -p sqleq-fuzz`. It
generates instances, runs both statements, and reports the first divergence. Its own soundness
rules — what it must never call a counterexample — are in
[`sqleq-fuzz/README.md`](../sqleq-fuzz/README.md).

## The Lean axis (`sqleq-lean/`, `lean/`)

Opt-in too, because it runs a Lean toolchain: `cargo build -p sqleq-lean`, with `lake` on `PATH`. It
proves one class of `INSERT` pair under the gather rule and has the Lean kernel check each proof
([LEAN.md](LEAN.md)).

| path | what |
|---|---|
| `sqleq-lean/src/main.rs`, `lib.rs` | the binary, and the pipeline from a pair to a kernel verdict |
| `sqleq-lean/src/{recognize,schema,translate}.rs` | read each `INSERT` into the parts the checker models, read the target table's columns at the precision the rule needs, and apply the checker's preconditions |
| `sqleq-lean/src/{emit,run}.rs` | write a batch of pairs as one Lean file, run Lean on it, and read off which pairs the kernel accepted |
| `lean/Sqleq/{Source,Check,Gather,Witness}.lean` | the Lean library: what an `INSERT` source denotes, the checker, its soundness proof, and the non-vacuity witnesses |
| `lean/SqleqTest/` | the checker's controls, built by `lake build SqleqTest` |

## Repository tools (`tools/`)

See [`tools/README.md`](../tools/README.md): the licence-file generator and its CI check, the IR
bridge sources above, and `lean_replay.py`, which replays the Lean axis's proved pairs on a real
Postgres.
