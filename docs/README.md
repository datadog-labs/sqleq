# Documentation

Start with the [README](../README.md): what the tool is, and how to run it with `sqleq-check`, the
one command that drives every backend. This directory is what to read next, in roughly the order
it is useful.

## Running it

| doc | what it answers |
| --- | --- |
| [../sqleq-check/README.md](../sqleq-check/README.md) | `sqleq-check`, the command you run: pair files or a corpus CSV in, per-axis answers and a CI exit code out — the flags, the pinned-pair suite, the portfolio's combined verdict, and long corpus runs. |
| [../tests/pairs/README.md](../tests/pairs/README.md) | The pinned pairs: pairs whose truth is known, with every axis's last answer, and how to add one. |

## What a verdict is worth

| doc | what it answers |
| --- | --- |
| [SOUNDNESS.md](SOUNDNESS.md) | What a proof rests on: what the frontend refuses rather than guess, and the assumptions it cannot check. |
| [VALIDATION.md](VALIDATION.md) | How the claim "sound" is checked rather than asserted — the cross-check between the provers and the disprover, and the defects it has caught. |

Read SOUNDNESS if you are about to trust a verdict. Read VALIDATION if you want to know why you
should.

## How it is built

| doc | what it answers |
| --- | --- |
| [DESIGN.md](DESIGN.md) | Why the frontend is a single Rust binary that refuses what it cannot lower, and the premise and four decisions behind it. History, kept for the reasoning. |
| [INTERNALS.md](INTERNALS.md) | The module map: which file does what, for reading or changing the code. |
| [SQLSOLVER.md](SQLSOLVER.md) | `sqleq-solver`, a Rust rewrite of SQLSolver's proof engine that proves from the same IR as the QED prover, and the JVM SQLSolver kept as its backup cross-check — what each does, and the defects that shaped them. |
| [../sqleq-fuzz/README.md](../sqleq-fuzz/README.md) | The refuting axis: how a counterexample is produced, and the rules that keep one honest. |
| [LEAN.md](LEAN.md) | The Lean axis: `INSERT … VALUES` vs `INSERT … SELECT * FROM unnest(…)` under the gather rule — what a proof claims, what it assumes, and what the kernel checks. |
| [logo/](logo/) | The `⊢≡` mark, its variants, and the scripts that render them. |

## Unfiled upstream notes

Two bugs found in third-party code, written up so the finding is not lost. Both are **drafts that
were never filed** with the projects they concern. Publishing them here is disclosure, not a bug
report: neither has had upstream review, and each may be wrong about that project's intent.

| doc | where |
| --- | --- |
| [prover-cvc5-brokenpipe.md](prover-cvc5-brokenpipe.md) | The QED prover crashes on large SMT formulas. |
| [sqlparser-is-distinct-from.md](sqlparser-is-distinct-from.md) | `sqlparser` 0.62 parsed the right operand of `IS [NOT] DISTINCT FROM` at precedence 0, so it swallowed a following `AND`. Fixed upstream in 0.63, which this workspace uses; `src/normalize.rs` still refuses the shapes the mis-parse produced, should it ever come back. |
