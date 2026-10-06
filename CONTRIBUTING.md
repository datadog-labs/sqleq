# Contributing

Thanks for looking. This is a research codebase that happens to ship a CLI, so a few of its
conventions are not the ones you would guess from the code — this file is the short list of those.

## Build and test

```sh
cargo test                 # the frontend: 206 + 10 + 7 + 2 + 190 + 19 + 11 + 11; sqleq-check: 59 + 16 + 12 + 5 + 2 + 12 + 12
cargo test -p sqleq-fuzz   # the disprover: 53 + 52 + 5
cargo test -p sqleq-solver # sqleq-solver, a Rust rewrite of SQLSolver: 61 (compiles Z3, see below)
cargo test -p sqleq-lean   # the Lean axis: 34 + 6 (needs Lean, see below)
cargo build -p sqleq-fuzz -p sqleq-solver
target/debug/sqleq-check --expect pinned --axes frontend,fuzz,sqleq-solver tests/pairs examples/*.sql
```

`cargo test` covers the frontend and `sqleq-check`, the command that runs every backend. Among the
frontend's suites is `tests/doc_links.rs`, which fails on any relative link in a Markdown file that
resolves to nothing; among `sqleq-check`'s is the hygiene gate over every committed pair
(`sqleq-check/tests/real_cases.rs`). The last line runs the
[pinned pairs](tests/pairs/README.md) on the frontend, fuzz and sqleq-solver axes, the ones CI runs
through `sqleq-check`. It needs the `sqleq-frontend`, `sqleq-fuzz` and `sqleq-solver` binaries;
`cargo test -p sqleq-solver` builds no binary (that crate has no integration test), so the
`cargo build` line before it makes sure of both opt-in ones. The Lean pins are checked by
`cargo test -p sqleq-lean`, and the `qed` and `sqlsolver-jvm` pins only where those tools are
installed.

`cargo test` deliberately does not build `sqleq-fuzz` or `sqleq-solver`. The first build of
`sqleq-fuzz` downloads DuckDB's release library (~40 MB, cached in `target/`), and the first build
of `sqleq-solver` compiles Z3 from source, which takes minutes and needs cmake and a C++20
compiler — so the root manifest sets `default-members = [".", "sqleq-check"]` and both are
opt-in. `sqleq-lean` is opt-in too: it runs the Lean 4 toolchain that `lean/lean-toolchain` names,
with `lake` on `PATH` (elan installs it, or put a release's `bin` there yourself), and its
integration test fails rather than skips without it. `cd lean && lake build Sqleq SqleqTest`
builds the Lean library and checks its controls. CI splits its jobs along the same lines — the
frontend and `sqleq-check` share one, and each opt-in crate has its own — and fetches Lean from a
digest-pinned release archive.

### Licensing

[`LICENSE-3rdparty.csv`](LICENSE-3rdparty.csv) lists every third-party component, and CI fails when
it no longer matches `Cargo.lock`. After any dependency change, regenerate it:

```sh
cargo install dd-rust-license-tool --version 1.0.6 --locked   # once
sh tools/update_license_3rdparty.sh
```

Components that are not crates (the DuckDB and Z3 libraries, and what the SQLSolver bridge compiles
against) are listed by hand in [`tools/license-3rdparty-extra.csv`](tools/license-3rdparty-extra.csv).
Dependencies must be under a permissive licence: `cargo deny --workspace check licenses` enforces
the allow-list in [`deny.toml`](deny.toml), and a licence outside it needs a discussion first.

Every `.rs`, `.py`, `.java`, `.toml`, `.yml`, `.sql` and `.sh` file opens with this header (after
the shebang, if there is one), written in the file's own comment syntax; CI checks the first six
lines of each:

```
// Unless explicitly stated otherwise all files in this repository are licensed under the
// Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/).
// Copyright 2026-Present Datadog, Inc.
```

## Lints

`cargo clippy --workspace --all-targets -- -D warnings` must be clean, and CI enforces it per
crate. So must `RUSTDOCFLAGS='-D warnings' cargo doc --no-deps`, which is the only thing that checks
intra-doc links: CI builds the frontend's docs as published, with `--features internals`, and with
`--document-private-items`, and `sqleq-lean`'s as published and with private items.

**`cargo fmt` is not used and must not be added to CI.** There is no `rustfmt.toml`, over a
thousand source lines already run past 100 columns, and `src/dml.rs` has never been formatted — a
format gate would rewrite the tree rather than check it. Match the wrap width of the file you are
editing (most are 100 columns; a few are a little wider) and leave the rest alone.

Where a lint is deliberately wrong for the code, annotate it with `#[allow(...)]` and one line
saying why, rather than rewriting the code to satisfy it. `src/types.rs` and
`sqleq-fuzz/src/duck.rs` have worked examples.

## Two rules that are not negotiable

These are the load-bearing ones. Both were bought with a bug, and both are easy to break while
making something better.

**1. Refuse, never emit best-effort IR.** If the frontend cannot lower a construct faithfully, it
must error and emit nothing — never approximate, never fall through to a more general path that
happens to typecheck. A prover fed a subtly wrong translation returns a *proof* of the wrong
theorem, and nothing downstream can tell. A refusal costs coverage, which is measurable; a silent
mis-lowering costs soundness, which is not. See [`docs/DESIGN.md`](docs/DESIGN.md) §5 and
[`docs/VALIDATION.md`](docs/VALIDATION.md), *Refuse, never guess*.

**2. Anything that grows the provable set needs the fuzz-axis cross-check — and the run is its own
control.** A change that makes more pairs provable is exactly the shape of a change that makes
unsound pairs provable, so the claim is not "N more proofs" but "N more proofs and zero of them
carries a counterexample". Run the provers and `sqleq-fuzz` over the same corpus and report the
cross-tab — one run does both:

```sh
sqleq-check --portfolio --expect report-only --corpus corpus.csv --json out.json
```

The cell where a prover says *equivalent* and `sqleq-fuzz` says *here is a counterexample* is the
alarm, and it is also the control: `--portfolio` reports such a case as `alarm` and fails the run
even under `report-only`. Do **not** add a separate hand-built negative-control batch — in a run
over every axis that control set already *is* a cell of the table, and building a second one
invites reporting the easy half. See [`docs/VALIDATION.md`](docs/VALIDATION.md) for why the run is
its own control.

The [pinned pairs](tests/pairs/README.md) are not that batch, and passing them is not that evidence.
They are a regression pin: each records what every axis said about a pair whose truth is already
known, so that a defect found once is checked on every axis from then on. A pinned pair can only
fail in a way someone has already seen. Do not grow them into a control set for one change; add the
pair that change fixed, and run the corpus cross-check as well.

## Documentation

[`docs/README.md`](docs/README.md) is the index: which document answers which question. The split
worth knowing before you write anything is between a **method** and a **measurement**. Methods and
durable defects are documentation: they stay true, and a reader can check them against the code. A
per-version count is not, because the corpora these tools get run against are not in this
repository, so nobody reading can reproduce the figure or tell when it stopped being true. If a
paragraph's force comes from such a count — a coverage percentage, a row total, a "this closed N
pairs" — rewrite it so the engineering reason survives without the figure.

Two consequences:

* The documentation carries no benchmark numbers at all.
* No default may resolve on one machine only. `sqleq-check` finds each backend by its flag, then
  `--bin-dir` for the binaries this repository builds, then the backend's environment variable
  (`$SQLEQ_FRONTEND`, `$SQLEQ_FUZZ`, `$SQLEQ_SOLVER_BIN`, `$SQLEQ_LEAN`, `$QED_PROVER`), then
  `PATH`, then this checkout's own `target/` build, the newer of release and debug; the QED
  prover's last resort is the newest Nix-built prover in `/nix/store`, and `sqleq-lean` skips
  `PATH` and takes release before debug. A flag, a variable or a `--bin-dir` that names no
  executable is an error, not a fallback — except that `sqleq-lean`'s `--lean-bin` and `$SQLEQ_LEAN`
  fall through to the next candidate. No step is a path that exists on one developer's machine
  only. An input with no such default —
  the SQLSolver fork's tree and its dependency directory — must be named (`--sqlsolver-tree` or
  `$SQLEQ_SQLSOLVER`, and `$SQLEQ_SQLSOLVER_DEPS`), and leaving it out is an error that names
  both. A machine-specific default fails as *"nothing there"* rather than as *"you did not say
  where"*, and the first is much more expensive to debug.

## Changes we are most interested in

* **Lowering coverage.** Most undecided pairs in any run are undecided because the frontend refused
  them, not because a prover gave up. `sqleq-check --corpus <corpus.csv> --axes frontend --expect
  report-only` counts the refusals by kind; for the finer list, the frontend's own corpus mode,
  `sqleq-frontend --csv <corpus.csv> -o <dir>`, prints the top refusal reasons, bucketed by
  construct, at the end of the run. That list is the work queue.
* **Counterexample quality** in `sqleq-fuzz` — instance generation that reaches cases the current
  generators do not, without ever manufacturing a witness that is not one.
* **Defects in the method itself.** [`docs/VALIDATION.md`](docs/VALIDATION.md) lists what this
  approach has caught; an argument for something it would miss is more valuable than a patch.

## Pinning a regression

A fix for a defect that can be stated as a pair — a false proof, a false refutation, two queries
that lowered alike — comes with that pair under [`tests/pairs/`](tests/pairs/README.md). In short:

1. Minimize it on an invented schema. Nothing in it may come from a corpus that is not public.
2. Write its `truth`, its `origin`, and a `witness` (not equivalent) or an `argument` (equivalent).
3. Rebuild every binary, then run `sqleq-check --expect pinned --bless --axes <list> <pair.sql>`
   naming every axis you have: `--axes` defaults to `frontend,qed`, and a run takes one SQLSolver,
   so `sqlsolver-jvm` is blessed in a second run beside `sqleq-solver`.
4. Read the diff against `truth`, and show the pair fails on a build from before the fix.
5. In the pull request, say why every pin that moved, moved.

## Reporting a soundness bug

A pair where a prover claims equivalence and the queries are not equivalent is the most serious bug
this project can have. Open an issue with the two queries, the DDL, and the counterexample if you
have one — it does not need to be minimized. The
[soundness bug template](https://github.com/datadog-labs/sqleq/issues/new?template=soundness_bug.md)
asks for exactly that. The fix will add the pair, minimized, to [`tests/pairs/`](tests/pairs/README.md).
