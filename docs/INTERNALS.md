# Internals: where things live

Orientation for someone reading or changing the code. It is not needed to *use* `sqleq` — for that,
start at the [README](../README.md).

sqleq is one crate. The frontend is its root, `src/*.rs`; each other axis is a module under `src/`,
with its binary in `src/bin/`, behind a cargo feature when its build is heavy (`fuzz` and `solver`,
on by default, and `lean` and `cvc5`). `sqleq-check` sits on top and runs every backend as a
subprocess. The frontend parses
a SQL pair, resolves names and types, and lowers it to the `Relation`/`Expr` IR the provers read —
`sqleq-solver`, a Rust rewrite of SQLSolver and the default prover, through the jobs
`src/sqlsolver.rs` packages that IR into, the QED prover directly, and `sqleq-cvc5`, the
experimental axis that encodes the same IR over cvc5's bags or sets of tuples. The other backends
read the pair themselves:
`sqleq-fuzz` runs the two statements on PostgreSQL looking for a counterexample and uses no part
of the frontend, and `sqleq-lean` parses with the frontend's parser but builds its own Lean terms.
See [DESIGN.md](DESIGN.md) for why the frontend refuses what it cannot lower.

## The frontend (`src/*.rs`)

| path | what |
|---|---|
| `src/lib.rs` | public API: `lower_sql`, and `lower_with`/`lower_with_ddl` over a `CatalogSource`; the reflexivity check (`reflexive`); the opt-in `internals` module that `sqleq-lean` and the benchmark harness link |
| `src/main.rs` | the `sqleq-frontend` CLI: one pair, a corpus CSV (`--csv`), or one lowered plan packaged as a SQLSolver job (`--sqlsolver --ir`) |
| `src/{catalog,scope,types,casts,lower,error}.rs` | catalog, de-Bruijn scope, type mapping/coercion, cast rules, lowering, errors |
| `src/{normalize,dml}.rs` | equivalence-preserving rewrites; `DELETE`, `UPDATE` and `INSERT` pairs reduced to queries computing their effect |
| `src/{infer,pgddl}.rs` | type inference (the `--infer`/`--infer-seeded` modes) and the raw-Postgres-DDL reader |
| `src/split.rs` | SQL text cut into statements where Postgres cuts it, on sqlparser's tokens; raw DDL is read through it, and `sqleq-check` decides a refused pair's triviality with it |
| `src/params.rs` | `$N` handling and the misalignment check ([SOUNDNESS.md](SOUNDNESS.md)) |
| `src/verify.rs` | re-derives the de-Bruijn invariants on every lowering — a second net under the soundness argument |
| `src/corpus.rs` | the corpus CSV reader and `--csv` batch driver: row *n* is case `pair{n:04}`, lowered against its own DDL; `sqleq-check --corpus` reads rows through it |
| `src/sqlsolver.rs` | packages a lowered plan as the job both SQLSolver implementations read, `sqleq-solver` and the JVM fork, with the schema as MySQL DDL ([SQLSOLVER.md](SQLSOLVER.md)) |
| `src/ir.rs` | the IR read back: the `Input` JSON as typed Rust, shape-checked, and tier 0 (`identical_sides`). `sqleq-solver` and `sqleq-cvc5` both read plans through it; no feature gates it |

| test | what |
|---|---|
| `tests/lower.rs` | the public `lower_sql` API: parse/lower shape and the soundness refusals |
| `tests/soundness.rs` | lowerings that used to make two different queries look alike to a prover, each with a control |
| `tests/coverage.rs` | expressions lowered by naming what they compute: what must lower alike, what must not, what is still refused |
| `tests/temporal.rs` | DATE/TIME/TIMESTAMP/TIMESTAMPTZ/INTERVAL kept apart in the IR, and every crossing between them named |
| `tests/depth.rs` | long and deeply nested predicates stay within a prover's nesting limit |
| `tests/reflexive.rs` | the reflexivity check: that it reaches past a lowering refusal, and never widens one into a proof |
| `tests/refusal_kinds.rs` | a refusal's kind against what Postgres makes of the input: `schema` where it rejects it, `unsupported` where it accepts it |
| `tests/resolution.rs` | name resolution, metamorphically: seed queries and mutants that change what a name resolves to must not meet, and mutants that keep it must still lower |
| `tests/doc_links.rs` | every relative link in every Markdown file resolves |
| `tests/pairs/` | pinned pairs: known truth, each axis's last answer, run by `sqleq-check --expect pinned` ([README](../tests/pairs/README.md)) |

Unit tests sit beside the code they test, in each module's `#[cfg(test)]` block — those for the
`DELETE`/`UPDATE`/`INSERT` reductions are in `src/dml.rs`.

## The command (`src/check/`)

Pair files, a directory of them, or a corpus CSV in; per-axis answers, a report and a CI exit code
out ([manual](check.md)). It uses the frontend's corpus reader and statement splitter, so a corpus
row is named and split exactly as every other tool reads it, but runs every backend as a
subprocess, so it needs none of their features and imports none of their modules.

| path | what |
|---|---|
| `src/bin/sqleq-check.rs`, `src/check/cli.rs` | entry point and the command line (clap) |
| `src/check/app.rs` | setup, the passes, `--jsonl` streaming and `--resume`, and the exit-code policy |
| `src/check/discover.rs` | finding each backend binary, and noticing one built from older sources |
| `src/check/inputs.rs` | which files are cases, what each is called, and whether a pair is `x` against `x` |
| `src/check/case.rs` | one case through the frontend and, when asked, the QED prover, and its SQLSolver job packaged |
| `src/check/axes/{fuzz,solver,cvc5,lean}.rs` | the axes that run after the frontend's pass: `sqleq-fuzz`, the SQLSolver axis (`sqleq-solver`, the default prover, or the JVM fork), the experimental cvc5 axis (`sqleq-cvc5`, one process per case), and `sqleq-lean` |
| `src/check/portfolio.rs` | `--portfolio`: every backend at once on each case, under one deadline, and one combined verdict |
| `src/check/suite.rs` | the pinned-pair header grammar, lint, judgement and `--bless` behind `--expect pinned` |
| `src/check/pinned.rs` | each axis's answer reduced to the one word a pin records |
| `src/check/report.rs` | the summary and per-axis tables it prints, and the `--json` and `--csv` files |
| `src/check/proc.rs` | subprocesses in their own process groups, with a hard wall-clock timeout and Ctrl-C handling |
| `src/check/util.rs` | temporary directories, `abspath`, `which`, an executable check |
| `tests/check/through_main.rs` | the binary end to end over stand-in backends, including the controls a fake prover must fail |
| `tests/check/{portfolio,corpus}.rs` | `--portfolio` and `--corpus` end to end, over stand-in backends |
| `tests/check/real_cases.rs` | the hygiene gate over every committed pair: it lints, carries the licence, and names nothing from a corpus or a machine |
| `tests/check/real_solver.rs` | the SQLSolver pass against the real `sqleq-solver`; skips without a build unless `$SQLEQ_SOLVER_BIN` is set |
| `tests/check/real_cvc5.rs` | the cvc5 axis against the real `sqleq-cvc5`; skips without a build unless `$SQLEQ_CVC5_BIN` is set |
| `tools/sqlsolver/` | our side of the IR bridge to the JVM SQLSolver, `sqleq-solver`'s backup cross-check: `IrToRel.java` builds the plan pair, `IrDriver.java` runs it; `sqleq-check --sqlsolver-jvm` compiles both ([SQLSOLVER.md](SQLSOLVER.md)) |

Running the provers and `sqleq-fuzz` over one corpus and cross-tabulating the answers is what
produces the table [VALIDATION.md](VALIDATION.md) describes, including the cell that is a soundness
alarm. `sqleq-check --portfolio` does that per case and reports the alarm as a verdict that fails
the run; `--corpus` points it, or any other run, at a corpus CSV instead of pair files.

## The SQLSolver rewrite (`src/solver/`)

A Rust rewrite of SQLSolver's proof engine, and the prover `sqleq-check` asks by default. It is the
`solver` feature, on by default; its first build compiles Z3 from source. It reads the same `Input`
JSON the QED prover gets, through `src/ir.rs`, and answers in the
JVM driver's row format, so the original SQLSolver, kept as its backup cross-check, can be swapped
in. What it rewrites, and where it deliberately differs from the original, is in
[SQLSOLVER.md](SQLSOLVER.md).

| path | what |
|---|---|
| `src/bin/sqleq-solver.rs` | the binary, behind `IrDriver`'s command line and job/result JSONL |
| `src/solver/{translate,uterm}.rs` | the typed IR's translation into U-expressions |
| `src/solver/{normalize,ic,alpha}.rs` | normalization, integrity-constraint rewrites, and the alpha-equivalence decision |
| `src/solver/{prove,setsolver}.rs` | the decision ladder, and the Z3 set solver as its last rung |
| `src/solver/eval.rs` | a concrete evaluator, so tests can check a rewrite against data |
| `tests/solver/` | the ladder end to end over hand-built IR, and the binary on plans past its depth bound |
| `examples/solver/` | manual gates and diagnostics run over a job file: translation, the ladder, rung-3 statistics, normalization traces and checks. CI compiles them but runs none, since their inputs are not in the repository |

## The cvc5 axis (`src/cvc5/`)

The lowered IR as an SMT problem over cvc5's theory of bags (or sets) of tuples, after the LPAR'24
Java translator: the `cvc5` feature, off by default because it links a cvc5 library.
`sh tools/build_cvc5.sh` builds the cvc5 branch it needs and the axis. It reads the IR through
`src/ir.rs`, as `sqleq-solver` does, so the two axes read one plan one way without the cvc5 build
compiling Z3, and answers in `IrDriver`'s row format with a `model` on `NEQ`.
[CVC5.md](CVC5.md) has the encoding and its readings of the types.

| path | what |
|---|---|
| `src/bin/sqleq-cvc5.rs` | the binary, behind `IrDriver`'s command line and job/result JSONL, plus `--plan`, `--semantics`, `--opt` and `--smt2` |
| `src/cvc5/translate.rs` | the encoding: sorts, tables and their constraints, each relational operator, expressions under three-valued logic, subqueries, and the model read back |
| `build.rs` | with the `cvc5` feature, puts `CVC5_LIB_DIR` in the binary's rpath, when that directory exists |
| `tests/cvc5/encoding.rs` | the encoding end to end over hand-built IR: proofs, exact and candidate models, refusals, and where bags and sets part |

## The refuting axis (`src/fuzz/`)

The `fuzz` feature, on by default; its first build downloads DuckDB and the PostgreSQL it runs pairs
on (`build.rs`). It generates instances, runs both statements, and reports the first divergence. Its own soundness
rules — what it must never call a counterexample — are in
[`docs/fuzz.md`](fuzz.md).

## The Lean axis (`src/lean/`, `lean/`)

The `lean` feature, off by default because it runs a Lean toolchain: `cargo build --features lean`,
with `lake` on `PATH`. It
proves one class of `INSERT` pair under the gather rule and has the Lean kernel check each proof
([LEAN.md](LEAN.md)).

| path | what |
|---|---|
| `src/bin/sqleq-lean.rs`, `src/lean/mod.rs` | the binary, and the pipeline from a pair to a kernel verdict |
| `src/lean/{recognize,schema,translate}.rs` | read each `INSERT` into the parts the checker models, read the target table's columns at the precision the rule needs, and apply the checker's preconditions |
| `src/lean/{emit,run}.rs` | write a batch of pairs as one Lean file, run Lean on it, and read off which pairs the kernel accepted |
| `lean/Sqleq/{Source,Check,Gather,Witness}.lean` | the Lean library: what an `INSERT` source denotes, the checker, its soundness proof, and the non-vacuity witnesses |
| `lean/SqleqTest/` | the checker's controls, built by `lake build SqleqTest` |

## Repository tools (`tools/`)

See [`tools/README.md`](../tools/README.md): the licence-file generator and its CI check, the IR
bridge sources above, and `lean_replay.py`, which replays the Lean axis's proved pairs on a real
Postgres.
