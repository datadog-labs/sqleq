# <picture><source media="(prefers-color-scheme: dark)" srcset="docs/logo/mark-dark.svg"><img src="docs/logo/mark-light.svg" alt="" height="44"></picture> sqleq

Decides whether two SQL queries are equivalent, meaning they always return the same results. Aims to
return a **proof** when they are equivalent or a **concrete counterexample** when they are not.
Turns query rewrites, optimizer changes, and migrations into something you verify rather than trust.

```
                              ┌─▶ sqleq-frontend ──▶ IR ───┬─▶ qed-prover ────▶ proof?
                              │   parse·resolve·type·lower └─▶ sqleq-solver ──▶ proof?
SQL pair + DDL ─▶ sqleq-check ┼─▶ sqleq-fuzz ──▶ generated tables in Postgres ▶ counterexample?
                              └─▶ sqleq-lean ──▶ Lean kernel ─────────────────▶ proof? (INSERTs)
```

**`sqleq-check`** is the command you run. It runs each backend you ask for as a process of its own
over the pairs you give it, and reports what each answered. With `--portfolio` it runs them on each
pair at once, under one deadline, and gives one combined verdict.

A proof and a counterexample are different claims, reached by different machinery, so the paths are
independent — which is what lets them check each other. Any pair a prover calls equivalent *and*
`sqleq-fuzz` refutes is a bug in one of them: `sqleq-check` reports it as an `alarm` and fails the
run, with or without `--portfolio`. See [`docs/VALIDATION.md`](docs/VALIDATION.md).

The provers are the [QED](https://github.com/qed-solver/prover) prover, an external binary, and
**`sqleq-solver`**, a Rust rewrite of [SQLSolver](https://github.com/SJTU-IPADS/SQLSolver)'s proof
engine that reads the same IR and builds with Cargo alone, Z3 included (compiled from source). Asked
with `--sqleq-solver`, it is a second opinion reported beside the QED prover's, and the default
`--expect equivalent` policy ignores it; in a portfolio its proof counts like any other. The
original Java SQLSolver can be asked instead (`--sqlsolver-jvm`), as a backup cross-check that is
being retired, but it needs a fork that is not published. See
[`docs/SQLSOLVER.md`](docs/SQLSOLVER.md).

The **Lean axis** (`sqleq-lean`) covers one class of `INSERT` pair the other axes cannot even state:
`INSERT … VALUES` against `INSERT … SELECT * FROM unnest(…)`, where a parameter is a scalar on one
side and an array on the other. It proves such a pair under an explicit rule for how the two sides'
parameters correspond, and has the Lean kernel check the proof. It needs a Lean toolchain;
`sqleq-check --lean` asks it, and outside `--expect pinned` its answer changes no exit code. See
[`docs/LEAN.md`](docs/LEAN.md).

## Quick start

Build `sqleq-check` and the two backends Cargo builds by itself:

```sh
cargo build --release                               # sqleq-frontend and sqleq-check
cargo build --release -p sqleq-fuzz -p sqleq-solver # the refuter and the in-repo prover
```

The first build of `sqleq-fuzz` downloads DuckDB's release library (~40 MB) and the PostgreSQL 17 it
runs pairs on (~12 MB, for Linux and macOS on x86_64 and arm64); the first build of `sqleq-solver`
compiles Z3 from source, which takes minutes and needs cmake and a C++20 compiler.
See [Building](#building).

Two pairs ship in [`examples/`](examples/), in the [input format](#input-format) every pair uses: a
rewrite that unnests an `IN` subquery into a join, which is equivalent only because the schema
declares a key, and a rewrite that drops a predicate, which is not equivalent.

```sh
CHECK=./target/release/sqleq-check
$CHECK --portfolio --axes fuzz,sqleq-solver --expect report-only -v examples/
```

```
  ✗ not-equivalent                   dropped_filter.sql    0.39s  by fuzz 0.39s  — users=[('0','2','2'); (NULL,'1','0'); ('1','2','0')]
  ✓ equivalent                       in_to_join.sql        1.92s  by sqleq-solver 0.01s
  …
  Portfolio  — fuzz, sqleq-solver on each case at once, 60s deadline
  ────────────────────────────────────────
  ✗ not-equivalent                     1
  ✓ equivalent                         1
```

The first case is actionable on its own: three rows of `users`, on which the two queries disagree.
The second is proved, on every instance the schema admits. (Output is cut here: a run starts by
naming the binary it found for each backend and the axes it asks, and `…` stands for the summary and
per-axis tables.)

**Adding the QED prover.** It is not vendored here; see [Building](#building) for getting it with
Nix. Once it is on `PATH`, drop `--axes`: by default a portfolio asks the frontend, the QED prover,
`sqleq-solver` and `sqleq-fuzz` (`--lean` adds the Lean axis).

```sh
$CHECK --portfolio --expect report-only -v examples/
```

```
  ✗ not-equivalent                   dropped_filter.sql    0.38s  by fuzz 0.38s  — users=[('0','2','2'); (NULL,'1','0'); ('1','2','0')]
  ✓ equivalent                       in_to_join.sql        1.96s  by sqleq-solver 0.02s, qed 0.11s
  …
  proved by     qed alone 0 · sqleq-solver alone 0 · both 1
```

**In CI.** Without `--portfolio`, `sqleq-check` asks the frontend and the QED prover, and by default
exits nonzero unless every pair is proved — so a directory of rewrites you expect to be safe is a
test:

```sh
$CHECK examples/
```

```
  ✗ unprovable           dropped_filter.sql    0.09s  complete-frag

  Summary
  ────────────────────────────────────────
  ✓ provable                 1
  ✗ unprovable               1
  ────────────────────────────────────────
  proved        1/2  (50.0%)
  …
```

The exit code is 1 here because `dropped_filter.sql` is unprovable — correctly, since it is not
equivalent. `--json
out.json` writes every case's answers as one machine-readable document.

## What a verdict means

These are the words `sqleq-check` reports: a status per pair in the default mode, an answer per
axis, and under `--portfolio` one verdict per pair.

| you get | from | it means | it does not mean |
|---|---|---|---|
| `provable`, `proved` | the QED prover (the default mode's status), any prover | equivalent on **every** instance the schema admits | — (but see [Parameters](#parameters-n) and [what a proof assumes](docs/SOUNDNESS.md)) |
| `proved-literal`, `emit-reflexive`, `reflexive` | a prover, the frontend | the two sides are one query once normalized, so equivalent without a proof search | — |
| `proved-gather` | Lean | equivalent under the gather rule for parameters, a different binding from the others' ([`docs/LEAN.md`](docs/LEAN.md)) | equivalent under index binding |
| `counterexample` | `sqleq-fuzz` | not equivalent, and here is the instance that shows it | — |
| `no-counterexample` | `sqleq-fuzz` | no divergence on the instances it tried | **not** a proof |
| `unprovable`, `no-proof` | the QED prover, any prover | this prover did not get there | **not** "not equivalent" |
| `timeout` | any axis | it ran out of time | **not** a claim about the pair |
| `refused`, `no-plan`, `unsupported` | the frontend, a prover | it declined to lower the pair rather than guess | **not** a claim about the pair |

Under `--portfolio` each pair gets one verdict: `alarm` (a proof *and* a counterexample under the
same parameter binding — one backend is wrong), `not-equivalent`, `equivalent`,
`equivalent-gather` or `equivalent-gather-generated` (the Lean axis's claims), `timeout` (nothing
decisive, and a backend was cut off) or `undecided`.

Two consequences worth internalizing before you read any output:

* **Undecided is a common and correct outcome.** Query equivalence is undecidable in general, so no
  tool decides every pair. On realistic workloads many pairs come back undecided.
* **"No proof" is not "not equivalent."** `dropped_filter.sql` happens to be both, and that is known
  only because `sqleq-fuzz` independently refuted it. A prover's silence is a statement about its
  reach, not about your queries.

Exit codes: `0` the `--expect` policy is satisfied, `1` it is not (and any `alarm`, whatever the
policy), `2` a usage or setup error such as a missing backend, `130`/`143` interrupted.

## What it supports

Everything below is exercised by the test suite (`cargo test`; mostly `tests/lower.rs`, with the
statement reductions in `src/dml.rs`).

| area | supported |
|---|---|
| **`FROM`** | base tables, inner / left / right / full outer / cross joins, parenthesized joins, `JOIN ... USING`, derived tables, `VALUES`, `FROM`-less `SELECT` |
| **projection** | arbitrary expressions, `*` and `t.*`, `DISTINCT`, `DISTINCT ON` |
| **grouping** | `GROUP BY` (including `(a, b)` row constructors), aggregates, aggregate `FILTER (WHERE …)`, `HAVING`, and Postgres **functional dependence** — a non-grouped column, or a whole expression, accepted because a declared key it reads is grouped on |
| **ordering** | `ORDER BY` with `LIMIT`, `OFFSET` and `FETCH FIRST`, as a slice of the sorted input ([`docs/SOUNDNESS.md`](docs/SOUNDNESS.md) states the assumption this rests on); past a `GROUP BY`, a `DISTINCT` or a set operation, only when every `ORDER BY` key is an output column |
| **subqueries** | non-correlated, correlated, and scalar |
| **set operations** | `UNION`, `UNION ALL`, `INTERSECT`, `EXCEPT` |
| **expressions** | `CASE`, `IN`-lists, row-`IN`, row-constructor comparison, `LIKE`/`ILIKE`/`SIMILAR TO` (as uninterpreted predicates), `BETWEEN`, `CAST`, `IS [NOT] DISTINCT FROM`, the `IS TRUE/FALSE/UNKNOWN` family, comparison type coercion |
| **schema** | keys and per-column nullability read from the DDL: `PRIMARY KEY`, `UNIQUE` (unless `DEFERRABLE`), `NOT NULL` |
| **statements** | a pair of `SELECT`s; also a pair of `DELETE`s, of `UPDATE`s, or of `INSERT`s into one table under one explicit column list, each reduced to the query computing its effect |

What it does **not** support, it refuses by name — window functions, `INTERSECT`/`EXCEPT ALL`,
`LATERAL`, set-returning functions in scalar position, `FETCH … WITH TIES`, `DISTINCT ON` over an
aggregate query, and others — rather than lowering something it cannot lower faithfully. That trade
is the whole soundness argument, and [`docs/SOUNDNESS.md`](docs/SOUNDNESS.md) makes it.

## Parameters (`$N`)

A pair with `$1, $2, …` in it lowers each `$N` to one shared symbol: the same value everywhere it
appears, and the same on both sides. So a proof about a parameterized pair is a proof under **index
binding** — `$1` on the left is `$1` on the right *because they share a number*.

That is what you meant only if both queries were numbered from the same call site. A rewrite that
drops or reorders a placeholder renumbers the rest, and the frontend cannot see your call site to
know. It detects and reports what it can (`parameter-misaligned`) and assumes the rest;
[`docs/SOUNDNESS.md`](docs/SOUNDNESS.md) is precise about what that does and does not license.

## Usage

```sh
CHECK=./target/release/sqleq-check

# One pair, or a directory of them (recursed): the frontend and the QED prover, and a CI exit code.
$CHECK pair.sql
$CHECK rewrites/

# Name the axes to ask: frontend, fuzz, qed, sqleq-solver, sqlsolver-jvm, lean.
$CHECK --axes frontend,qed,sqleq-solver --expect report-only rewrites/

# The frontend, QED prover, sqleq-solver and sqleq-fuzz at once on each pair, one combined
# verdict per pair, under a shared 60-second deadline (--lean adds the Lean axis).
$CHECK --portfolio -t 60 rewrites/

# A corpus CSV (rows of query A, query B, DDL) instead of files, each row against its own DDL,
# writing each case to a JSONL file as soon as it is answered (--resume continues a stopped run).
$CHECK --portfolio --expect report-only --corpus corpus.csv --catalog inferred-seeded \
    --jsonl results.jsonl
```

`--catalog inferred-seeded` reads tables and columns from the DDL but *infers* parameter types,
which is what lets a bare `$N` lower against a declared schema; the default, `declared`, refuses
one.
`sqleq-check` has its own manual: [`sqleq-check/README.md`](sqleq-check/README.md).

Each backend is also a binary of its own, documented with its crate. `sqleq-check` takes each from
its flag, `--bin-dir`, its environment variable, `PATH` or this repository's `target/`, in that
order (the manual has the details). The frontend alone lowers a pair to the prover's Input JSON
(`sqleq-frontend pair.sql out.json`), and is a library too:

```rust
let input_json = sqleq_frontend::lower_sql(sql_text)?; // serde_json::Value, the prover's Input
```

## Input format

A single file containing, in order:

1. zero or more `CREATE TABLE` statements (the schema; `UNIQUE`/`PRIMARY KEY` become prover keys,
   unless `DEFERRABLE`),
2. zero or more `declare {scalar,aggregate} function NAME(args) returns TYPE;` lines (a small DSL
   for typing opaque/parameterized functions; stripped before SQL parsing),
3. exactly **two** statements — the pair to compare. Normally two `SELECT`s; a pair of `DELETE`s,
   of `UPDATE`s or of `INSERT`s is also accepted.

`-- key: value` comment lines at the top of a file are optional; the pinned pairs in
[`tests/pairs/`](tests/pairs/README.md) and in `examples/` use them to record what each axis
answers.

## Building

Recent stable Rust (edition 2021; MSRV 1.88, set by the locked dependency tree). A bare
`cargo build --release` builds the frontend and `sqleq-check`; the frontend depends only on
`sqlparser`, `serde_json` and `csv`. `sqleq-check` builds no backend: it runs each one as a
subprocess, and the heavier ones are opt-in workspace members, each built with `-p`:

* **`sqleq-fuzz`** — `cargo build --release -p sqleq-fuzz`. Its first build downloads DuckDB's
  release library (~40 MB) and a prebuilt PostgreSQL 17 (~12 MB, digest-pinned) for Linux and macOS
  on x86_64 and arm64, which it runs pairs on. To link a libduckdb you already have instead, set
  `DUCKDB_LIB_DIR`; the build script checks it before it considers downloading anything. To run
  pairs on a PostgreSQL 17 of your own, set `SQLEQ_PG_BIN` to its `bin` directory, and
  `SQLEQ_PG_DOWNLOAD=0` to skip the download. On Linux the fetched PostgreSQL uses the system's
  OpenSSL 3, libxml2, Kerberos, zstd and lz4 libraries; where one is missing it cannot start, and
  `sqleq-fuzz` uses the `postgres` on `PATH` instead.
* **`sqleq-solver`** — `cargo build --release -p sqleq-solver`. Its first build compiles Z3 from
  source, which takes minutes and needs cmake and a C++20 compiler. Z3 is linked in statically, so
  the binary needs nothing at run time.
* **`sqleq-lean`** — `cargo build --release -p sqleq-lean`. At run time it needs `lake` (from a Lean
  toolchain, e.g. via [elan](https://github.com/leanprover/elan)) on `PATH` or in `$LAKE`, and
  builds the [`lean/`](lean/) package on first use; that package pins its own Lean version and
  needs no Mathlib.

The QED prover is external (the upstream [`qed-solver`](https://github.com/qed-solver/prover)
project) and is not vendored here. It needs the native `z3` and `cvc5` solvers, and its Nix flake
bundles them, so Nix is the most reliable way to get a working prover without installing solvers
yourself:

```sh
nix --extra-experimental-features nix-command --extra-experimental-features flakes \
    shell github:qed-solver/prover
```

The `--extra-experimental-features` flags enable `nix-command`/`flakes` where they aren't on by
default. `sqleq-check` takes the prover from `--prover`, `$QED_PROVER` or `PATH`, in that order,
and failing those the newest Nix-built `qed-prover` it can find.

## Documentation

| doc | what |
|---|---|
| [`docs/`](docs/README.md) | the index — start here if you don't know which of the below you want |
| [`sqleq-check/README.md`](sqleq-check/README.md) | the `sqleq-check` manual: every flag, the portfolio, corpus runs, the output |
| [`docs/SOUNDNESS.md`](docs/SOUNDNESS.md) | what a verdict rests on: what is refused, and what is assumed |
| [`docs/VALIDATION.md`](docs/VALIDATION.md) | how the tool is validated, and the defects that validation has caught |
| [`docs/DESIGN.md`](docs/DESIGN.md) | why the frontend is built this way |
| [`docs/SQLSOLVER.md`](docs/SQLSOLVER.md) | `sqleq-solver` on the same IR as the QED prover, and the Java SQLSolver that cross-checks it |
| [`docs/LEAN.md`](docs/LEAN.md) | the Lean axis: which `INSERT` pairs it proves, and under what rule |
| [`docs/INTERNALS.md`](docs/INTERNALS.md) | module map, for reading or changing the code |
| [`tests/pairs/README.md`](tests/pairs/README.md) | the pinned pairs: known truths, every axis's answer, and how to add one |
| [`sqleq-fuzz/README.md`](sqleq-fuzz/README.md) | the disproving axis, and its own soundness rules |
| [`tools/README.md`](tools/README.md) | the scripts that are not part of any crate |

## Contributing

[`CONTRIBUTING.md`](CONTRIBUTING.md) — how to build and test, the lint policy, and the two rules a
change must not break: *refuse rather than emit best-effort IR*, and *anything that grows the
provable set is cross-checked against the refuting axis, where the run is its own control*.
[`SECURITY.md`](SECURITY.md) is how to report a vulnerability.

## License

Apache-2.0 — see [`LICENSE`](LICENSE) and [`NOTICE`](NOTICE). The `qed-prover` binary and SQLSolver
are separate upstream projects, not vendored here, each under its own licence. Third-party
components and their licences are listed in [`LICENSE-3rdparty.csv`](LICENSE-3rdparty.csv).
