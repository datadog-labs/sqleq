# <picture><source media="(prefers-color-scheme: dark)" srcset="docs/logo/mark-dark.svg"><img src="docs/logo/mark-light.svg" alt="" height="44"></picture> sqleq

Decides whether two SQL queries are equivalent, meaning they always return the same results. Aims to
return a **proof** when they are equivalent or a **concrete counterexample** when they are not.
Turns query rewrites, optimizer changes, and migrations into something you verify rather than trust.

```
                   ┌─▶ sqleq-frontend ──▶ Input JSON ─┬─▶ qed-prover ──▶ provable?
SQL pair + DDL ────┤   (parse·resolve·type·lower)     └─▶ sqleq-solver ─▶ proved?
                   └─▶ sqleq-fuzz ──▶ DuckDB tables ─────────────────────▶ counterexample?
```

A proof and a counterexample are different claims, reached by different machinery, so the paths are
independent — which is what lets them check each other. Any pair a prover calls equivalent *and*
`sqleq-fuzz` refutes is a bug in one of them; see [`docs/VALIDATION.md`](docs/VALIDATION.md).

The second prover is **`sqleq-solver`**, a Rust rewrite of
[SQLSolver](https://github.com/SQLSolver/SQLSolver)'s proof engine that reads the same Input JSON and
builds from this repository against a Z3 you supply. It is a second opinion: `tools/sqleq_check.py
--sqlsolver` reports its answer beside the prover's, and it never changes a verdict or an exit code.
The original Java SQLSolver can be asked instead, as a backup cross-check, but that needs a fork of
it that is not published. See [`docs/SQLSOLVER.md`](docs/SQLSOLVER.md).

The **Lean axis** (`sqleq-lean`) is narrower still. It decides one class of `INSERT` pair the other
axes cannot even state: `INSERT … VALUES` against `INSERT … SELECT * FROM unnest(…)`, where a
parameter is a scalar on one side and an array on the other. It proves such a pair under an explicit
rule for how the two sides' parameters correspond, and has the Lean kernel check the proof. It
needs a Lean toolchain, and `tools/sqleq_check.py --lean` runs it as a second opinion that never
changes an exit code. See [`docs/LEAN.md`](docs/LEAN.md).

## Quick start

The two things you might want have very different prerequisites, so pick first:

| you want | run | needs |
|---|---|---|
| a **counterexample** — *is this rewrite wrong?* | `sqleq-fuzz file pair.sql` | nothing external |
| a **proof** — *is this rewrite safe?* | `sqleq-frontend pair.sql out.json`, then `qed-prover` | Nix, for the prover |

Both take the [same input file](#input-format), and both examples below ship in
[`examples/`](examples/) — one pair is an equivalent rewrite, the other is not.

**Look for a counterexample.** Nothing to install first: the build fetches DuckDB's own release
library and links against it.

```sh
cargo build --release -p sqleq-fuzz      # first build downloads libduckdb (~40 MB)
FUZZ=./target/release/sqleq-fuzz

$FUZZ file examples/dropped_filter.sql   # B drops an org-scoping predicate
# NOT-EQUIVALENT
# counterexample: users=[(0,2,2); (NULL,1,0); (1,2,0)]

$FUZZ file examples/in_vs_or.sql         # `tier IN (1,2)` against an OR of equalities
# NO-COUNTEREXAMPLE
```

The first is actionable on its own: three rows of `users`, and the two queries disagree on them.

**Look for a proof.** Needs `qed-prover` on your `PATH` — see [Building](#building).

```sh
cargo build --release                    # the frontend; skips sqleq-fuzz by default
FE=./target/release/sqleq-frontend
mkdir -p out

$FE examples/in_vs_or.sql       out/in_vs_or.json
$FE examples/dropped_filter.sql out/dropped_filter.json
qed-prover out/ | grep Equivalence
```

```
Equivalence is not provable for dropped_filter.json
Equivalence is provable for in_vs_or.json
```

The machine-readable verdict is the `provable` field of the `.result` file written beside each
input:

```sh
$ python3 -c "import json; print(json.load(open('out/in_vs_or.result'))['provable'])"
True
```

## What a verdict means

| you get | from | it means | it does not mean |
|---|---|---|---|
| `Equivalence is provable` | a proving axis | equivalent on **every** instance the schema admits | — (but see [Parameters](#parameters-n)) |
| `NOT-EQUIVALENT` + a counterexample | `sqleq-fuzz` | not equivalent, and here is the instance that shows it | — |
| `NO-COUNTEREXAMPLE` | `sqleq-fuzz` | no divergence on the instances it tried | **not** a proof |
| `Equivalence is not provable` | a proving axis | this prover did not get there | **not** "not equivalent" |
| an error naming a construct | the frontend | it declined to lower the pair rather than guess | **not** a claim about the pair |

Two consequences worth internalizing before you read any output:

* **Undecided is a common and correct outcome.** Query equivalence is undecidable in general, so no
  tool decides every pair. On a realistic corpus most pairs come back undecided.
* **"Not provable" is not "not equivalent."** The pair above is both, but only because `sqleq-fuzz`
  independently refuted it. A prover's silence is a statement about its reach, not about your
  queries.

## What it supports

Everything below is exercised by the test suite (`tests/lower.rs`, `cargo test`).

| area | supported |
|---|---|
| **`FROM`** | base tables, inner / left / right / full outer / cross joins, parenthesized joins, `JOIN ... USING`, derived tables, `VALUES`, `FROM`-less `SELECT` |
| **projection** | arbitrary expressions, `*` and `t.*`, `DISTINCT` |
| **grouping** | `GROUP BY` (including `(a, b)` row constructors), aggregates, aggregate `FILTER (WHERE …)`, `HAVING`, and Postgres **functional dependence** — a non-grouped column, or a whole expression, accepted because a declared key it reads is grouped on |
| **subqueries** | non-correlated, correlated, and scalar |
| **set operations** | `UNION`, `UNION ALL`, `INTERSECT`, `EXCEPT` |
| **expressions** | `CASE`, `IN`-lists, row-`IN`, row-constructor comparison, `LIKE`/`ILIKE`/`SIMILAR TO` (as uninterpreted predicates), `BETWEEN`, `CAST`, `IS [NOT] DISTINCT FROM`, the `IS TRUE/FALSE/UNKNOWN` family, comparison type coercion |
| **schema** | keys and per-column nullability read from the DDL: `PRIMARY KEY`, `UNIQUE`, `NOT NULL` |
| **statements** | a pair of `SELECT`s; also a pair of `DELETE`s or a pair of `UPDATE`s, reduced to the query computing their effect |

What it does **not** support, it refuses by name — `LIMIT`/`OFFSET`, window functions, `DISTINCT
ON`, `INTERSECT`/`EXCEPT ALL`, `LATERAL`, set-returning functions and others — rather than lowering
something it cannot lower faithfully. That trade is the whole soundness argument, and
[`docs/SOUNDNESS.md`](docs/SOUNDNESS.md) makes it.

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
# One pair → the prover's Input JSON:
./target/release/sqleq-frontend input.sql out.json

# A whole corpus CSV in one pass, each row lowered against its own DDL (no .sql intermediate):
./target/release/sqleq-frontend --csv corpus.csv -o out/ --report report.json --infer-seeded

# A corpus of .sql pairs → verdicts + a CI exit code:
python3 tools/sqleq_check.py --expect report-only -j 8 corpus/
```

`--infer-seeded` reads columns from the DDL but *infers* parameter types, which is what lets `$N`
placeholders lower at all. The batch harness has its own manual:
[`tools/README.md`](tools/README.md).

As a library:

```rust
let input_json = sqleq_frontend::lower_sql(sql_text)?; // serde_json::Value, the prover's Input
```

## Input format

A single file containing, in order:

1. zero or more `CREATE TABLE` statements (the schema; `UNIQUE`/`PRIMARY KEY` become prover keys),
2. zero or more `declare {scalar,aggregate} function NAME(args) returns TYPE;` lines (a small DSL
   for typing opaque/parameterised functions; stripped before SQL parsing),
3. exactly **two** statements — the pair to compare. Normally two `SELECT`s; a pair of `DELETE`s or
   a pair of `UPDATE`s is also accepted.

## Building

Recent stable Rust (edition 2021; MSRV 1.85, set by the dependency tree). The frontend depends only
on `sqlparser`, `serde_json` and `csv`.

`sqleq-fuzz` is in the same Cargo workspace but outside `default-members`, so a bare `cargo build
--release` deliberately skips it — its first build downloads DuckDB's release library (~40 MB).
Build it with `cargo build --release -p sqleq-fuzz`. To link a libduckdb you already have instead,
set `DUCKDB_LIB_DIR`; the build script checks it before it considers downloading anything.

`sqleq-solver` is outside `default-members` too, because it links a Z3 you supply:
`$SQLEQ_Z3_LIB_DIR` names the directory holding `libz3.so` and `$Z3_SYS_Z3_HEADER` a `z3.h` from the
same release. Then `cargo build --release -p sqleq-solver`; the library's location is baked into the
binary.

The prover binary is external (from the upstream `qed-solver` project) and is not vendored here. It
needs the native `z3` and `cvc5` solvers, and its Nix flake bundles them, so Nix is the most
reliable way to get a working prover without installing solvers yourself:

```sh
nix --extra-experimental-features nix-command --extra-experimental-features flakes \
    shell github:qed-solver/prover
```

The `--extra-experimental-features` flags enable `nix-command`/`flakes` where they aren't on by
default. Every `qed-prover` command above assumes it is on `PATH` this way.

## Documentation

| doc | what |
|---|---|
| [`docs/`](docs/README.md) | the index — start here if you don't know which of the below you want |
| [`docs/SOUNDNESS.md`](docs/SOUNDNESS.md) | what a verdict rests on: what is refused, and the one assumption |
| [`docs/VALIDATION.md`](docs/VALIDATION.md) | how the tool is validated, and the defects that has caught |
| [`docs/DESIGN.md`](docs/DESIGN.md) | why the frontend is built this way |
| [`docs/SQLSOLVER.md`](docs/SQLSOLVER.md) | `sqleq-solver`, the second prover on the same IR, and the Java SQLSolver that cross-checks it |
| [`docs/INTERNALS.md`](docs/INTERNALS.md) | module map, for reading or changing the code |
| [`tools/README.md`](tools/README.md) | the batch harness |
| [`sqleq-fuzz/README.md`](sqleq-fuzz/README.md) | the disproving axis, and its own soundness rules |

## Contributing

[`CONTRIBUTING.md`](CONTRIBUTING.md) — how to build and test, the lint policy, and the two rules a
change must not break: *refuse rather than emit best-effort IR*, and *anything that grows the
provable set is cross-checked against the refuting axis, where the run is its own control*.
[`SECURITY.md`](SECURITY.md) is how to report a vulnerability.

## License

Apache-2.0 — see [`LICENSE`](LICENSE) and [`NOTICE`](NOTICE). The `qed-prover` binary and SQLSolver
are separate upstream projects, not vendored here, each under its own licence. Third-party components
and their licences are listed in [`LICENSE-3rdparty.csv`](LICENSE-3rdparty.csv).
