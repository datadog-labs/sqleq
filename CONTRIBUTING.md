# Contributing

Thanks for looking. This is a research codebase that happens to ship a CLI, so a few of its
conventions are not the ones you would guess from the code — this file is the short list of those.

## Build and test

```sh
cargo test --no-default-features   # the frontend and sqleq-check: no Z3, DuckDB or PostgreSQL to build
cargo test                         # adds sqleq-fuzz and sqleq-solver, the default features (see below)
cargo test --features lean         # adds the Lean axis (needs Lean, see below)
cargo test --features cvc5         # adds the cvc5 axis (after `sh tools/build_cvc5.sh`, see docs/CVC5.md)
cargo test --features duckdb       # adds sqleq-fuzz's DuckDB engine and the tests that drive it
cargo build
target/debug/sqleq-check --expect pinned --axes frontend,fuzz,sqleq-solver tests/pairs examples/*.sql
```

`cargo test --no-default-features` covers the frontend and `sqleq-check`, the command that runs
every backend. Among the
frontend's suites is `tests/doc_links.rs`, which fails on any relative link in a Markdown file that
resolves to nothing; among `sqleq-check`'s is the hygiene gate over every committed pair
(`tests/check/real_cases.rs`). The last line runs the
[pinned pairs](tests/pairs/README.md) on the frontend, fuzz and sqleq-solver axes, the ones CI runs
through `sqleq-check`. It needs the `sqleq-frontend`, `sqleq-fuzz` and `sqleq-solver` binaries,
which the `cargo build` line before it builds: `fuzz` and `solver` are default features. The Lean
pins are checked by `cargo test --features lean`, and the `qed`, `sqlsolver-jvm` and `cvc5` pins
only where those tools are installed.

`sqleq-fuzz` runs pairs on a PostgreSQL 17: the one `SQLEQ_PG_BIN` names, else the one its build
fetched (for Linux and macOS on x86_64 and arm64; `SQLEQ_PG_DOWNLOAD=0` skips the download), else
the `postgres` on `PATH`. Its pinned pairs need one, as does `sqleq-check --axes fuzz`, and
`cargo test` tests the Postgres engine against it, skipping those tests without one
unless `SQLEQ_PG_REQUIRED` is set, as CI sets it. CI uses the fetched one. The engine builds a template cluster on its first run, in
`~/.cache/sqleq` unless `SQLEQ_PG_CACHE` says otherwise.

sqleq is one crate, with a cargo feature for each axis whose build is heavy. The defaults, `fuzz`
and `solver`, make a bare `cargo build` or `cargo test` slow the first time: the `fuzz` feature
downloads the PostgreSQL it runs on (the DuckDB engine and its ~40 MB library are the off-by-default
`duckdb` feature), and
the `solver` feature compiles Z3 from source, which takes minutes and needs cmake and a C++20
compiler. `--no-default-features` builds neither, and is the quick loop for the frontend and
`sqleq-check`. The `lean` feature is off by default: it runs the Lean 4 toolchain that
`lean/lean-toolchain` names, with `lake` on `PATH` (elan installs it, or put a release's `bin`
there yourself), and its integration test fails rather than skips without it.
`cd lean && lake build Sqleq SqleqTest` builds the Lean library and checks its controls. The `cvc5`
feature is off by default too; it links a cvc5 library ([docs/CVC5.md](docs/CVC5.md)). CI splits
its jobs along the same lines — the frontend and `sqleq-check` share one, with
`--no-default-features`, and each feature has its own — and fetches Lean from a digest-pinned
release archive.

### Licensing

[`LICENSE-3rdparty.csv`](LICENSE-3rdparty.csv) lists every third-party component, and CI fails when
it no longer matches `Cargo.lock`. After any dependency change, regenerate it:

```sh
cargo install dd-rust-license-tool --version 1.0.6 --locked   # once
sh tools/update_license_3rdparty.sh
```

Components that are not crates (the DuckDB and Z3 libraries, and what the SQLSolver bridge compiles
against) are listed by hand in [`tools/license-3rdparty-extra.csv`](tools/license-3rdparty-extra.csv).
Dependencies must be under a permissive licence: `cargo deny check licenses` enforces
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
`--document-private-items`, and every other crate's as published and with private items.

The code must also compile with the oldest Rust the manifests declare, their `rust-version`, which
CI's `msrv` job checks. The lockfile sets that floor: a dependency bump that needs a newer Rust
has to raise `rust-version` in every `Cargo.toml` and `MSRV` in `.github/workflows/ci.yml`
together.

**`cargo fmt` is not used and must not be added to CI.** There is no `rustfmt.toml`, over a
thousand source lines already run past 100 columns, and `src/dml.rs` has never been formatted — a
format gate would rewrite the tree rather than check it. Match the wrap width of the file you are
editing (most are 100 columns; a few are a little wider) and leave the rest alone.

Where a lint is deliberately wrong for the code, annotate it with `#[allow(...)]` and one line
saying why, rather than rewriting the code to satisfy it. `src/types.rs` and
`src/fuzz/duck.rs` have worked examples.

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
  construct, at the end of the run. That list is the work queue, and its `unsupported` part is
  the queue proper: queries Postgres runs that the frontend declines, the frontend's own parser
  included. A `parse` or `schema` refusal is input Postgres rejects too.
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
   naming every axis you have: `--axes` defaults to `frontend,sqleq-solver`, and a run takes one SQLSolver,
   so `sqlsolver-jvm` is blessed in a second run beside `sqleq-solver`.
4. Read the diff against `truth`, and show the pair fails on a build from before the fix.
5. In the pull request, say why every pin that moved, moved.

## Reporting a soundness bug

A pair where a prover claims equivalence and the queries are not equivalent is the most serious bug
this project can have. Open an issue with the two queries, the DDL, and the counterexample if you
have one — it does not need to be minimized. The
[soundness bug template](https://github.com/datadog-labs/sqleq/issues/new?template=soundness_bug.md)
asks for exactly that. The fix will add the pair, minimized, to [`tests/pairs/`](tests/pairs/README.md).

## Releasing

Releases are published to crates.io by
[`.github/workflows/release.yml`](.github/workflows/release.yml), with no stored token: crates.io's
trusted publishing exchanges the workflow's OIDC token for one that lasts 30 minutes.

1. In one pull request, set the new `version` in `Cargo.toml` (and `Cargo.lock`, which a build
   updates) and add its entry to [`CHANGELOG.md`](CHANGELOG.md).
2. Once it merges, publish a GitHub release for the tag `v<version>` on the merge commit, with the
   changelog entry as its notes. Publishing it creates the tag: an organization ruleset protects
   tags, and workflows cannot create them.
3. The release starts the workflow, in the organization's `protected-main-env` environment, which
   only `main` and tags can deploy to. It checks that the tag names the version, verifies the
   package, uploads it, and then installs it from crates.io on a clean runner and runs one pair it
   must prove and one it must refute.

A published version cannot be deleted, only yanked, so a mistake is fixed by the next version.

Before the first release, once: the first version uploaded by hand with a short-lived token scoped
to the crate (a trusted publisher can only be added to a crate that exists); then, on crates.io, this
repository, `release.yml` and the `protected-main-env` environment registered as the crate's trusted
publisher, the token revoked, and trusted publishing made required. The environment and the tag
protection are the organization's, so the repository needs no settings of its own.
