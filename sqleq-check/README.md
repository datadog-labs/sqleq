# sqleq-check

The batch harness: a directory of `.sql` pairs in, verdicts and a CI exit code out.

```
.sql ──▶ sqleq-frontend ──▶ .json ──▶ qed-prover ──▶ verdict ──▶ consolidated report
```

Point it at `.sql` files or directories; it lowers each, proves equivalence of the two queries
inside, classifies the outcome, and prints a summary with timings — plus machine-readable JSON/CSV
and CI-friendly exit codes. It also consumes **pre-parsed `.json`** plans directly (e.g. the
prover's bundled `tests/calcite/` corpus), skipping the lowering stage. More axes can be asked about
the same pairs: [sqleq-solver](#second-opinion-sqleq-solver), [sqleq-fuzz](../sqleq-fuzz/README.md)
and the [Lean axis](../docs/LEAN.md).

Every backend runs as a subprocess, in a process group of its own, so this crate builds none of
them: it is light enough to be a default workspace member, and a timeout or a Ctrl-C kills a
backend together with whatever it started (the prover's z3 and cvc5, Lean under `lake`).

## Requirements

- `sqleq-check` and `sqleq-frontend` — `cargo build --release` in this repo builds both, as
  `target/release/sqleq-check` and `target/release/sqleq-frontend`; the harness finds the frontend
  there (then in `target/debug`) on its own.
- `qed-prover` — the external prover binary.

Resolution order is `--frontend`/`--prover` flags, then `$SQLEQ_FRONTEND`/`$QED_PROVER`, then
`PATH`. Beyond that the frontend falls back to this repo's build and the prover to the newest
wrapped binary under `/nix/store` (that wrapper carries z3 + cvc5 on its own `PATH`, so it works
outside the Nix dev shell).

## Input format

Each `.sql` file must contain, in order: `CREATE TABLE` statements, optional
`declare {scalar,aggregate} function` lines, and **exactly two** `SELECT` statements. See
[`../examples/`](../examples/) for two runnable pairs. To build them in bulk from a CSV corpus, use
the frontend's own `--csv` mode.

An already-lowered `.json` plan is also a first-class case: the harness skips the lowering stage and
hands it straight to the prover. That is how an archived `Input` is re-checked, and it is also the
path `--sqleq-solver` was built around.

## Usage

```sh
# Validate a directory of known-equivalent rewrite pairs (CI):
sqleq-check rewrites/         # exit 1 if any pair isn't provably equiv

# Report over a corpus, 8 workers, machine-readable output:
sqleq-check --expect report-only -j 8 --json out.json corpus/

# Verbose per-case output, 30s/case, tighten each SMT request to 5s:
sqleq-check -v -t 30 --smt-timeout 5000 rewrites/

# Keep intermediates (.json/.result) for debugging:
sqleq-check --keep ./work rewrites/
```

`sqleq-check` here is `target/release/sqleq-check`, or `cargo run --release -p sqleq-check --`.
`--help` lists every flag; a long flag may be shortened to any unambiguous prefix (`--no-ret`).

| Flag | Meaning |
|------|---------|
| `-j, --jobs N` | Parallel cases (default `min(8, ncpu)`). Each case runs z3+cvc5, so don't oversubscribe heavily. |
| `-t, --timeout S` | Per-case wall-clock budget in seconds (default 60). On timeout the whole process group is killed. |
| `--smt-timeout MS` | Sets `QED_SMT_TIMEOUT` per SMT request (prover default is 10000 ms). |
| `--expect equivalent` | (default) Exit non-zero unless **every** case is `provable`. |
| `--expect report-only` | Always exit 0; just report. |
| `--expect pinned` | Each case's header pins every axis's answer; exit non-zero on any movement. See [Pinned pairs](#pinned-pairs). |
| `--axes LIST` | Which axes to run, comma-separated: `frontend`, `fuzz`, `qed`, `sqleq-solver`, `sqlsolver-jvm`, `lean` (default `frontend,qed`). A prover axis brings in `frontend`; at most one SQLSolver per run. `sqlsolver-rust`, sqleq-solver's axis before it was renamed, is still read as `sqleq-solver`. |
| `--bless` | With `--expect pinned`: rewrite each case's `expect` lines for the axes that ran. |
| `--fuzz-bin PATH` | The `sqleq-fuzz` binary (else `$SQLEQ_FUZZ`, `PATH`, or this repo's `target/{release,debug}`). |
| `--json` / `--csv FILE` | Write structured results (full prover `Stats` per case in JSON). |
| `--keep DIR` | Keep intermediates instead of using temp dirs. |
| `--no-retry` | Don't re-run transient failures serially at the end. |
| `--sqleq-solver` | Ask `sqleq-solver`, a Rust rewrite of SQLSolver, about the same cases too — see [Second opinion](#second-opinion-sqleq-solver). Never changes the exit code. |
| `--sqleq-solver-bin PATH` | The `sqleq-solver` binary (else `$SQLEQ_SOLVER_BIN`, `PATH`, or this repo's `target/{release,debug}`). |
| `--sqlsolver-jvm` | Ask the original SQLSolver instead, as a JVM fork through `tools/sqlsolver/IrDriver`: `sqleq-solver`'s backup cross-check. Same jobs, same result rows, same buckets. Not with `--sqleq-solver`. |
| `--sqlsolver-tree DIR` | With `--sqlsolver-jvm`: the fork to run it from, either as this flag or as `$SQLEQ_SQLSOLVER`. |
| `--sqleq-solver-timeout MS` | Per-row cap for the second opinion (default: `-t` in ms). Its own, because the provers are not comparably fast. |
| `--lean` | Also run the Lean axis, `sqleq-lean`, over the `.sql` cases: `INSERT … VALUES` vs `INSERT … SELECT * FROM unnest(…)` pairs, proved under the gather rule (or, with generated cells such as `DEFAULT`, its weaker generated form). It reads the pair files itself, so it answers pairs the frontend refuses. The same as adding `lean` to `--axes`; outside `--expect pinned` it never changes the exit code. See [`../docs/LEAN.md`](../docs/LEAN.md). |
| `--lean-bin PATH` | The `sqleq-lean` binary (else `$SQLEQ_LEAN`, or this repo's `target/{release,debug}`). It needs `lake` on `PATH`. |
| `-v` / `-q` | Verbose (every case) / quiet (summary only). Default shows non-provable cases + summary. |

## Status taxonomy

| Status | Meaning |
|--------|---------|
| `provable` | The prover proved the two queries equivalent. |
| `unprovable` | The prover ran but could not prove equivalence. |
| `refused` | The frontend would not lower the SQL. Sub-classified as `refuse_kind`: `parse` (sqlparser rejected the text), `unsupported` (a construct we decline to lower), `parameter-misaligned` (the two queries' `$N` do not line up), `schema` (unknown table/column, bad DDL, wrong number of queries). |
| `panic` | The prover panicked/crashed on the case. |
| `timeout` | Exceeded the per-case wall-clock budget. |
| `error` | Anything else (e.g. an unreadable result). |
| `lowered` | The frontend lowered the pair, and the qed axis was not asked (`--axes` without `qed`). |

> **`unprovable` is not `non-equivalent`.** QED is sound but not complete, so `unprovable` means
> "not proven equivalent" and nothing more. For a definite counterexample use
> [`../sqleq-fuzz/`](../sqleq-fuzz/), which evaluates both sides on random instances.

> **`refused` is a feature.** The prover is sound *given faithful IR*, so the frontend refuses
> anything it cannot lower faithfully rather than emitting best-effort IR. A refusal costs
> completeness; guessing would cost soundness.

## Trivial vs non-trivial, and the `capability` line

A pair is **trivial** when its two queries reach the prover identical. That is common here and it
is not the prover's doing: the frontend normalizes both sides, its normalizations are
equivalence-preserving rewrites, and on many pairs the rewrite one of them undoes *is* the
optimization the pair was written to exercise. Proving those is still sound — normalize soundly,
then prove — but the prover is confirming `x = x`, so counting them as capability can overstate it
by close to an order of magnitude.

So the summary prints two numbers, always together:

```
  proved        <proved>/<cases>         (<pct>%)
  capability    <proved>/<non-trivial>   (<pct>%)   pairs whose two queries differ
                <proved>/<trivial> of the reflexive (x vs x) pairs also proved
```

`capability` is the one to quote. Each case carries `trivial` and `trivial_basis` in the JSON and
CSV, and `meta.triviality` in the JSON holds the totals.

Triviality is decided on the **lowered IR** — `queries[0] == queries[1]` in the plan handed to the
prover (`trivial_basis: "ir"`). That is the exact thing the prover sees, so it also catches pairs
differing only in aliasing, quoting or whitespace. A refused case has no IR, so it falls back to
comparing the two statements in the source text after whitespace normalization
(`trivial_basis: "text"`), which is weaker; `trivial` is `null` when neither test applies.

## Second opinion: sqleq-solver

`--sqleq-solver` runs a second prover over **the same lowered plan** and prints a second table:
`sqleq-solver`, this repo's Rust rewrite of SQLSolver. With `--sqlsolver-jvm` instead, the original
SQLSolver answers, kept as a cross-check. It is off by default, and it cannot change the exit code.

```sh
sqleq-check --expect report-only --sqleq-solver -j 8 -t 30 corpus/
```

```
  Second opinion  — sqleq-solver, over the same lowered IR
  ────────────────────────────────────────
  proved                    27
  no-proof                   2
  unsupported                1
  ────────────────────────────────────────
  of pairs that differ      30
  both provers              25
  only the QED prover        2
  only sqleq-solver          2   what the second opinion adds
  neither                    1
```

The cells below the rule use **the same denominator as `capability`** — pairs whose two queries
actually differ — so the two tables can be read against each other. `only sqleq-solver` is the whole
reason the flag exists. The buckets are the `s_bucket` column of the JSON and CSV:

| bucket | meaning |
|---|---|
| `proved` | It proved the pair equivalent. The only bucket that is a claim. |
| `proved-literal` | Its tier 0: the two plans were *already identical*, so nothing was proved. Held apart from `proved` for the same reason this harness holds `trivial` apart from `capability`. |
| `no-proof` | It considered the pair and found no proof. |
| `unsupported` | **Ours, not theirs.** Either the frontend refused the row, or the bridge could not express the plan. The `s_note` column says which. |
| `timeout` | The cap ran out. Kept out of `no-proof` deliberately: that prover answers `UNKNOWN` when interrupted, so `killed` is the only thing separating "we stopped asking" from "they declined". |
| `error` | It threw. |
| `missing` | It never answered — the driver died before reaching the row. |

Two things about the numbers, both printed under the table on every run:

* **`no-proof` is not a refutation.** That prover's `NEQ` means "no proof found", exactly like its
  `UNKNOWN`; only `EQ` is a claim. `sqleq-fuzz` is the only disprover in this project.
* **The two opinions share a frontend.** The bridge hands the second prover the very `Input` JSON
  the QED prover reads — no SQL is emitted and nothing re-parses the case — which is what makes the
  comparison exact, and also what makes it *correlated*: a lowering bug yields the same wrong plan
  on both axes, so agreement corroborates the provers, not the lowering.

Mechanics worth knowing before reading a slow run:

- The rows go to one driver process **at the end, sequentially**, not per case. A JVM's startup
  would otherwise swamp the cases, and the per-row cap is load-sensitive — a second opinion that
  changes under `-j` is not one.
- A row can outlive its cap, so the driver writes what it has and then halts itself; the harness
  notices the missing answers and resumes. The wall line reports `N driver self-halt(s) in M
  pass(es)` when that happened.
- Setup is checked **before any case runs**: a missing binary, classpath or `javac` exits 2 with the
  fix rather than reporting `missing` for every row.
- `sqleq-solver` (the default) needs only its binary: `cargo build --release -p sqleq-solver`, which
  compiles Z3 from source and links it in, so nothing is needed at run time (the first build needs
  cmake and a C++20 compiler).
- With `--sqlsolver-jvm` it requires the fork tree (its `lib/` holds the Z3 natives), a JDK, and
  `$SQLEQ_SQLSOLVER_DEPS` pointing at the exploded dependency directory the fork was compiled
  against. The driver compiles itself on first use and recompiles when
  `tools/sqlsolver/IrDriver.java` or `IrToRel.java` is newer than the class. Both drivers take the
  same arguments and write the same rows, including the exit-3 self-halt.

See [`../docs/SQLSOLVER.md`](../docs/SQLSOLVER.md) for what `sqleq-solver` rewrites and where it
differs from the original, the bridge to the JVM fork and the Calcite-ectomy behind it, and the
false proofs that keep the fork a cross-check.

## Pinned pairs

`--expect pinned` is the policy for [`../tests/pairs/`](../tests/pairs/README.md): every case says
in its header whether it is equivalent, and what each axis answered when it was last reviewed. The
run asks the axes in `--axes` again and fails on any movement — an improvement as much as a
regression — and on any answer that contradicts the case's truth, which `--bless` will never pin.

```sh
# What CI checks, one axis per job:
sqleq-check --expect pinned --axes frontend,fuzz,sqleq-solver tests/pairs examples/*.sql
# After a change that moves answers: rewrite the pins, then review the diff.
sqleq-check --expect pinned --bless --axes frontend,fuzz,sqleq-solver tests/pairs examples/*.sql
```

Two things differ from the other policies:

* **The fuzz axis.** `fuzz` runs `sqleq-fuzz file` on each pair with its trial budget passed
  explicitly (`--trials 120 --rows 5 --seed 0`), so a change to the tool's defaults cannot move a
  pin. It reads the pair itself, so `--axes fuzz` alone needs no frontend.
* **The catalog header.** A case may say `-- catalog: inferred` or `-- catalog: inferred-seeded`,
  and the frontend is run with the matching flag; any `.sql` input may, not only a pinned one.

A binary in this repository's `target/` that is older than its sources gets a warning, and stops
`--bless` outright: pins blessed against a stale build record an older tree's answers. `--bless`
rewrites only `expect` lines, keeps each file's line endings (CRLF included) and final newline, and
replaces a file atomically, only when its bytes change.

## Exit codes

- `0` — policy satisfied (see `--expect`).
- `1` — policy not satisfied (some case failed the expectation).
- `2` — usage / setup error (no inputs, binary not found, a flag that does not combine, …).
- `130` — interrupted (Ctrl-C); every backend still running is killed first.

## Implementation notes

- Each case runs in an isolated working directory, so identically-named files in different folders
  don't collide and a panic in one case can't affect others.
- The `.result` JSON the prover writes is the source of truth for the verdict and timing breakdown —
  the prover's stdout `Debug` line has a known quirk where `provable`/`total_duration` are not
  populated.
- A zero exit from the frontend with no JSON on disk is still counted as `refused`. The exit code is
  reliable, but a case we cannot prove must never be silently dropped.
- Transient failures (`panic`/`timeout`/`error`) are retried once serially, so a heavy case starved
  under `-j` isn't misreported. Refusals are deterministic and never retried.
- `--json` writes `{meta, cases}`, one object per case with the fields of `Case` in
  [`src/case.rs`](src/case.rs); `--csv` writes the scalar ones, one row per case.

## Tests

`cargo test -p sqleq-check` runs the unit tests, the binary end to end over stand-in backends (shell
scripts, so no backend needs building), and the hygiene gate over every committed pair. The test
that drives the real `sqleq-solver` skips unless a build of it exists; with `$SQLEQ_SOLVER_BIN` set
(relative to the repository root, as CI sets it) a missing binary fails it instead.
