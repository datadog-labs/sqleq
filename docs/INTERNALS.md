# Internals: where things live

Orientation for someone reading or changing the code. It is not needed to *use* `sqleq` — for that,
start at the [README](../README.md).

The shape is one frontend feeding three axes. `src/` parses a SQL pair, resolves names and types,
and lowers it to the `Relation`/`Expr` IR that both proving axes consume — the QED prover directly,
and `sqleq-solver`, a Rust rewrite of SQLSolver, through the job files `src/sqlsolver.rs` writes.
The third axis, `sqleq-fuzz/`, reads no IR at all: it runs the two queries against DuckDB looking for
a counterexample. See
[DESIGN.md](DESIGN.md) for why it is arranged that way.

## The frontend (`src/`)

| path | what |
|---|---|
| `src/lib.rs` | public API (`lower_sql`) |
| `src/main.rs` | CLI |
| `src/{catalog,scope,types,casts,lower,error}.rs` | catalog, de-Bruijn scope, type mapping/coercion, cast rules, lowering, errors |
| `src/{normalize,dml}.rs` | equivalence-preserving rewrites; `DELETE`/`UPDATE` reduced to the query computing their effect |
| `src/{infer,pgddl}.rs` | type inference (the `--infer`/`--infer-seeded` modes) and the raw-Postgres-DDL reader |
| `src/params.rs` | `$N` handling and the misalignment check ([SOUNDNESS.md](SOUNDNESS.md)) |
| `src/verify.rs` | re-derives the de-Bruijn invariants on every lowering — a second net under the soundness argument |
| `src/corpus.rs` | the `--csv` batch driver: row *n* of the CSV is case `pair{n:04}`, lowered against its own DDL |
| `src/sqlsolver.rs` | the `sqlsolver` axis's job files, which `sqleq-solver` and the JVM SQLSolver both read ([SQLSOLVER.md](SQLSOLVER.md)) |
| `tests/lower.rs` | integration tests (parse/lower shape + soundness refusals) |
| `tests/reflexive.rs` | integration tests for the reflexivity check — that it reaches past a lowering refusal, and never widens one into a proof |

## The second proving axis (`sqleq-solver/`)

A Rust rewrite of SQLSolver's proof engine, opt-in because it links a Z3 you supply: `cargo build -p
sqleq-solver` with `$SQLEQ_Z3_LIB_DIR` and `$Z3_SYS_Z3_HEADER` set. It reads the same `Input` JSON the
QED prover gets and answers in the JVM driver's row format, so the original SQLSolver, kept as its
cross-check, can be swapped in. What it rewrites, and where it deliberately differs from the
original, is in [SQLSOLVER.md](SQLSOLVER.md).

## The disproving axis (`sqleq-fuzz/`)

A separate crate, opt-in because its first build downloads DuckDB: `cargo build -p sqleq-fuzz`. It
generates instances, runs both queries, and reports the first divergence. Its own soundness rules —
what it must never call a counterexample — are in [`sqleq-fuzz/README.md`](../sqleq-fuzz/README.md).

## The batch harnesses (`tools/`)

| path | what |
|---|---|
| `tools/sqleq_check.py` | a corpus of `.sql` pairs → verdicts + CI exit code, via frontend + prover ([manual](../tools/README.md)) |
| `tools/linkcheck.py` | every relative link in every tracked Markdown file resolves |
| `tools/sqlsolver/` | our side of the IR bridge to the JVM SQLSolver, `sqleq-solver`'s cross-check: `IrToRel.java` builds the plan pair, `IrDriver.java` runs it ([SQLSOLVER.md](SQLSOLVER.md)) |

Running all three axes over one corpus and cross-tabulating them is what produces the table
[VALIDATION.md](VALIDATION.md) describes, including the cell that is a soundness alarm. The driver
that does it is not in this repository — it exists to run corpora that are not public — but
`sqleq_check.py --sqlsolver` and `sqleq-fuzz` between them reach every axis it reaches.
