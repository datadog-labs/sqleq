# Contributing

Thanks for looking. This is a research codebase that happens to ship a CLI, so a few of its
conventions are not the ones you would guess from the code — this file is the short list of those.

## Build and test

```sh
cargo test                 # the frontend: 204 + 179 + 17 + 12
cargo test -p sqleq-fuzz   # the disprover: 53 + 52 + 5
python3 -m unittest discover -s tools -p 'test_*.py'
python3 tools/linkcheck.py # every relative link in every tracked Markdown file resolves
```

`cargo test` deliberately does not build `sqleq-fuzz`. Its first build downloads DuckDB's release
library (~40 MB, cached in `target/`), so the root manifest sets `default-members = ["."]` and the
disprover is opt-in. CI splits them into two jobs for the same reason.

### Licensing

[`LICENSE-3rdparty.csv`](LICENSE-3rdparty.csv) lists every third-party component, and CI fails when
it no longer matches `Cargo.lock`. After any dependency change, regenerate it:

```sh
cargo install dd-rust-license-tool --version 1.0.6 --locked   # once
sh tools/update_license_3rdparty.sh
```

Components that are not crates (the DuckDB library, and what the SQLSolver bridge compiles against)
are listed by hand in [`tools/license-3rdparty-extra.csv`](tools/license-3rdparty-extra.csv).
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
crate.

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
carries a counterexample". Run both axes over the same corpus and report the cross-tab; the cell
where a prover says *equivalent* and `sqleq-fuzz` says *here is a counterexample* is the alarm, and
it is also the control. Do **not** add a separate hand-built negative-control batch — in a two-axis
run that control set already *is* a cell of the table, and building a second one invites reporting
the easy half. See [`docs/VALIDATION.md`](docs/VALIDATION.md), *Each axis is the other's control*.

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
* No default may resolve on one machine only. Every path is an explicit flag, then a `$SQLEQ_*`
  environment variable, then an error naming the variable — or `required=True` when it is the run's
  primary input. A machine-specific default fails as *"nothing there"* rather than as *"you did not
  say where"*, and the first is much more expensive to debug.

## Changes we are most interested in

* **Lowering coverage.** Most undecided pairs in any run are undecided because the frontend refused
  them, not because a prover gave up. `sqleq-frontend --csv <corpus.csv> -o <dir>` prints the top
  refusal reasons, bucketed, at the end of the run; that list is the work queue.
* **Counterexample quality** in `sqleq-fuzz` — instance generation that reaches cases the current
  generators do not, without ever manufacturing a witness that is not one.
* **Defects in the method itself.** [`docs/VALIDATION.md`](docs/VALIDATION.md) lists what this
  approach has caught; an argument for something it would miss is more valuable than a patch.

## Reporting a soundness bug

A pair where a prover claims equivalence and the queries are not equivalent is the most serious bug
this project can have. Open an issue with the two queries, the DDL, and the counterexample if you
have one — it does not need to be minimized. The
[soundness bug template](https://github.com/datadog-labs/sqleq/issues/new?template=soundness_bug.md)
asks for exactly that.
