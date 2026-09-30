# `tools/`

| script | what it does |
|---|---|
| `sqleq_check.py` | the batch harness: a directory of `.sql` pairs in, verdicts and a CI exit code out |
| `linkcheck.py` | every relative link in every tracked Markdown file resolves |
| `update_license_3rdparty.sh` | regenerates `LICENSE-3rdparty.csv`; `--check` is the CI gate — see [`../CONTRIBUTING.md`](../CONTRIBUTING.md) |
| `sqlsolver/` | our side of the IR bridge to the second prover — see [`../docs/SQLSOLVER.md`](../docs/SQLSOLVER.md) |

Standard library only, Python 3.8+. `test_sqleq_check.py` covers `sqleq_check.py`; run it with
`python3 -m unittest discover -s tools -p 'test_*.py'`.

To lower a whole corpus CSV in one pass instead, the frontend has its own
`--csv` mode and needs no harness — see the repository [`README`](../README.md).

---

## sqleq-check

```
.sql ──▶ sqleq-frontend ──▶ .json ──▶ qed-prover ──▶ verdict ──▶ consolidated report
```

Point it at `.sql` files or directories; it lowers each, proves equivalence of
the two queries inside, classifies the outcome, and prints a summary with
timings — plus machine-readable JSON/CSV and CI-friendly exit codes. It also
consumes **pre-parsed `.json`** plans directly (e.g. the prover's bundled
`tests/calcite/` corpus), skipping the lowering stage.

There is no JVM in this pipeline, and no comparison against one. The first stage
used to be the Java/Calcite `qed-parser`; it is now the Rust frontend from this
repo, which is why the taxonomy below says `refused` rather than `parse_error`.
The Java parser is fully retired, along with the parity harness that compared
the two.

### Requirements

- Python 3.8+ (standard library only).
- `sqleq-frontend` — `cargo build --release` in this repo is enough; the harness
  finds `target/release` (then `target/debug`) on its own.
- `qed-prover` — the external prover binary.

Resolution order is `--frontend`/`--prover` flags, then `$SQLEQ_FRONTEND`/`$QED_PROVER`,
then `PATH`. Beyond that the frontend falls back to this repo's build and the
prover to the newest wrapped binary under `/nix/store` (that wrapper carries z3
+ cvc5 on its own `PATH`, so it works outside the Nix dev shell).

### Input format

Each `.sql` file must contain, in order: `CREATE TABLE` statements, optional
`declare {scalar,aggregate} function` lines, and **exactly two** `SELECT`
statements. See [`../examples/`](../examples/) for two runnable pairs. To build
them in bulk from a CSV corpus, use the frontend's own `--csv` mode.

An already-lowered `.json` plan is also a first-class case: the harness skips
the lowering stage and hands it straight to the prover. That is how an archived
`Input` is re-checked, and it is also the path `--sqlsolver` was built around.

### Usage

```sh
# Validate a directory of known-equivalent rewrite pairs (CI):
python3 tools/sqleq_check.py rewrites/         # exit 1 if any pair isn't provably equiv

# Report over a corpus, 8 workers, machine-readable output:
python3 tools/sqleq_check.py --expect report-only -j 8 --json out.json corpus/

# Verbose per-case output, 30s/case, tighten each SMT request to 5s:
python3 tools/sqleq_check.py -v -t 30 --smt-timeout 5000 rewrites/

# Keep intermediates (.json/.result) for debugging:
python3 tools/sqleq_check.py --keep ./work rewrites/
```

| Flag | Meaning |
|------|---------|
| `-j, --jobs N` | Parallel cases (default `min(8, ncpu)`). Each case runs z3+cvc5, so don't oversubscribe heavily. |
| `-t, --timeout S` | Per-case wall-clock budget in seconds (default 60). On timeout the whole process group is killed. |
| `--smt-timeout MS` | Sets `QED_SMT_TIMEOUT` per SMT request (prover default is 10000 ms). |
| `--expect equivalent` | (default) Exit non-zero unless **every** case is `provable`. |
| `--expect report-only` | Always exit 0; just report. |
| `--json` / `--csv FILE` | Write structured results (full prover `Stats` per case in JSON). |
| `--keep DIR` | Keep intermediates instead of using temp dirs. |
| `--no-retry` | Don't re-run transient failures serially at the end. |
| `--sqlsolver` | Ask SQLSolver about the same cases too — see [Second opinion](#second-opinion-sqlsolver). Never changes the exit code. |
| `--sqlsolver-impl {jvm,rust}` | Which SQLSolver to ask: the JVM fork through `tools/sqlsolver/IrDriver` (default), or this repo's Rust port, `sqleq-solver`. Same jobs, same result rows, same buckets. |
| `--sqlsolver-tree DIR` | With `jvm`: the fork to run it from, either as this flag or as `$SQLEQ_SQLSOLVER`. |
| `--sqlsolver-bin PATH` | With `rust`: the `sqleq-solver` binary (else `$SQLEQ_SOLVER_BIN`, `PATH`, or this repo's `target/{release,debug}`). |
| `--sqlsolver-timeout MS` | Per-row cap for that prover (default: `-t` in ms). Its own, because the two provers are not comparably fast. |
| `-v` / `-q` | Verbose (every case) / quiet (summary only). Default shows non-provable cases + summary. |

### Status taxonomy

| Status | Meaning |
|--------|---------|
| `provable` | The prover proved the two queries equivalent. |
| `unprovable` | The prover ran but could not prove equivalence. |
| `refused` | The frontend would not lower the SQL. Sub-classified as `refuse_kind`: `parse` (sqlparser rejected the text), `unsupported` (a construct we decline to lower), `schema` (unknown table/column, bad DDL, wrong number of queries). |
| `panic` | The prover panicked/crashed on the case. |
| `timeout` | Exceeded the per-case wall-clock budget. |
| `error` | Anything else (e.g. an unreadable result). |

> **`unprovable` is not `non-equivalent`.** QED is sound but not complete, so
> `unprovable` means "not proven equivalent" and nothing more. For a definite
> counterexample use [`../sqleq-fuzz/`](../sqleq-fuzz/), which evaluates both sides
> on random instances.

> **`refused` is a feature.** The prover is sound *given faithful IR*, so the
> frontend refuses anything it cannot lower faithfully rather than emitting
> best-effort IR. A refusal costs completeness; guessing would cost soundness.

### Trivial vs non-trivial, and the `capability` line

A pair is **trivial** when its two queries reach the prover identical. That is
common here and it is not the prover's doing: the frontend normalizes both
sides, its normalizations are equivalence-preserving rewrites, and on many pairs
the rewrite one of them undoes *is* the optimization the pair was written to
exercise. Proving those is still sound — normalize soundly, then prove — but the
prover is confirming `x = x`, so counting them as capability can overstate it
by close to an order of magnitude.

So the summary prints two numbers, always together:

```
  proved        <proved>/<lowered>       (<pct>%)
  capability    <proved>/<non-trivial>   (<pct>%)   pairs whose two queries differ
                <proved>/<trivial> of the reflexive (x vs x) pairs also proved
```

`capability` is the one to quote. Each case carries `trivial` and
`trivial_basis` in the JSON and CSV, and `meta.triviality` in the JSON holds the
totals.

Triviality is decided on the **lowered IR** — `queries[0] == queries[1]` in the
plan handed to the prover (`trivial_basis: "ir"`). That is the exact thing the
prover sees, so it also catches pairs differing only in aliasing, quoting or
whitespace. A refused case has no IR, so it falls back to comparing the two
statements in the source text after whitespace normalization
(`trivial_basis: "text"`), which is weaker; `trivial` is `null` when neither
test applies.

### Second opinion: SQLSolver

`--sqlsolver` runs a second prover over **the same lowered plan** and prints a
second table. It is off by default, and it cannot change the exit code.

```sh
python3 tools/sqleq_check.py --expect report-only --sqlsolver -j 8 -t 30 corpus/
```

```
  Second opinion  — SQLSolver, over the same lowered IR
  ────────────────────────────────────────
  s-proved                  27
  s-no-proof                 2
  s-unsupported              1
  ────────────────────────────────────────
  of pairs that differ      30
  both provers              25
  only qed-prover            2
  only SQLSolver             2   what the second opinion adds
  neither                    1
```

The cells below the rule use **the same denominator as `capability`** — pairs
whose two queries actually differ — so the two tables can be read against each
other. `only SQLSolver` is the whole reason the flag exists.

| bucket | meaning |
|---|---|
| `s-proved` | It proved the pair equivalent. The only bucket that is a claim. |
| `s-proved-literal` | Its tier 0: the two plans were *already identical*, so nothing was proved. Held apart from `s-proved` for the same reason this harness holds `trivial` apart from `capability`. |
| `s-no-proof` | It considered the pair and found no proof. |
| `s-unsupported` | **Ours, not theirs.** Either the frontend refused the row, or the bridge could not express the plan. The `s_note` column says which. |
| `s-timeout` | The cap ran out. Kept out of `s-no-proof` deliberately: that prover answers `UNKNOWN` when interrupted, so `killed` is the only thing separating "we stopped asking" from "they declined". |
| `s-error` | It threw. |
| `s-missing` | It never answered — the JVM died before reaching the row. |

Two things about the numbers, both printed under the table on every run:

* **`s-no-proof` is not a refutation.** That prover's `NEQ` means "no proof
  found", exactly like its `UNKNOWN`; only `EQ` is a claim. `sqleq-fuzz` is the
  only disprover in this project.
* **The two opinions share a frontend.** The bridge hands SQLSolver the very
  `Input` JSON the QED prover reads — no SQL is emitted and nothing re-parses
  the case — which is what makes the comparison exact, and also what makes it
  *correlated*: a lowering bug yields the same wrong plan on both axes, so
  agreement corroborates the provers, not the lowering.

Mechanics worth knowing before reading a slow run:

- The rows go to one JVM **at the end, sequentially**, not per case. Startup
  would otherwise swamp the cases, and the per-row cap is load-sensitive — a
  second opinion that changes under `-j` is not one.
- That prover can hang past an interrupt, so the driver writes the row and then
  halts the JVM; the harness notices the missing answers and resumes. The wall
  line reports `N JVM self-halt(s) in M pass(es)` when that happened.
- Setup is checked **before any case runs**, and a missing classpath or
  `javac` exits 2 with the fix rather than reporting `s-missing` for every row.
  With `--sqlsolver-impl=jvm` (the default) it requires the fork tree (its
  `lib/` holds the Z3 natives), a JDK, and `$SQLEQ_SQLSOLVER_DEPS` pointing at
  the exploded dependency directory the fork was compiled against. The driver
  compiles itself on first use and recompiles when `tools/sqlsolver/IrDriver.java`
  or `IrToRel.java` is newer than the class.
- With `--sqlsolver-impl=rust` it needs only the `sqleq-solver` binary:
  `cargo build --release -p sqleq-solver` with `$SQLEQ_Z3_LIB_DIR` (a
  directory holding `libz3.so`) and `$Z3_SYS_Z3_HEADER` (a matching `z3.h`) set.
  The library's location is baked into the binary, so nothing is needed at run
  time. It takes the same arguments and writes the same rows, including the
  exit-3 self-halt when a row outlives its cap and grace period.

See [`../docs/SQLSOLVER.md`](../docs/SQLSOLVER.md) for the bridge, the
Calcite-ectomy behind it, and what each bucket was measured to be worth.

### Exit codes

- `0` — policy satisfied (see `--expect`).
- `1` — policy not satisfied (some case failed the expectation).
- `2` — usage / setup error (no inputs, binary not found, …).

### Implementation notes

- Each case runs in an isolated working directory, so identically-named files in
  different folders don't collide and a panic in one case can't affect others.
- The `.result` JSON the prover writes is the source of truth for the verdict and
  timing breakdown — the prover's stdout `Debug` line has a known quirk where
  `provable`/`total_duration` are not populated.
- A zero exit from the frontend with no JSON on disk is still counted as
  `refused`. The exit code is reliable (unlike the Java parser's, which is why
  the old harness tested for a non-empty file instead), but a case we cannot
  prove must never be silently dropped.
- Transient failures (`panic`/`timeout`/`error`) are retried once serially, so a
  heavy case starved under `-j` isn't misreported. Refusals are deterministic
  and never retried.

