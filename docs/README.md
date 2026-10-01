# Documentation

Start with the [README](../README.md) — what the tool is, and how to run either entry point. This
directory is what to read next, in roughly the order it is useful.

## What a verdict is worth

| doc | what it answers |
| --- | --- |
| [SOUNDNESS.md](SOUNDNESS.md) | What a proof rests on: what the frontend refuses rather than guess, and the one assumption it cannot check. |
| [VALIDATION.md](VALIDATION.md) | How the claim "sound" is checked rather than asserted — the cross-check between a prover and a disprover, and the defects it has caught. |

Read SOUNDNESS if you are about to trust a verdict. Read VALIDATION if you want to know why you
should.

## How it is built

| doc | what it answers |
| --- | --- |
| [DESIGN.md](DESIGN.md) | Why the frontend is a single Rust binary that refuses what it cannot lower, and the premise and four decisions behind it. History, kept for the arguments. |
| [INTERNALS.md](INTERNALS.md) | The module map: which file does what, for reading or changing the code. |
| [SQLSOLVER.md](SQLSOLVER.md) | The second proving backend, reached through the same IR — how it is wired in, and the defects that shaped the wiring. |
| [../tools/README.md](../tools/README.md) | The batch harness: running a directory of pairs, and the CI exit code. |
| [../sqleq-fuzz/README.md](../sqleq-fuzz/README.md) | The refuting axis: how a counterexample is produced, and the rules that keep one honest. |
| [LEAN.md](LEAN.md) | The Lean axis: `INSERT … VALUES` vs `INSERT … SELECT * FROM unnest(…)` under the gather rule — what a proof claims, what it assumes, and what the kernel checks. |
| [logo/](logo/) | The `⊢≡` mark, its variants, and the script that renders them. |

## Unfiled upstream notes

Two bugs found in third-party code, written up so the finding is not lost. Both are **drafts that
were never filed** with the projects they concern. Publishing them here is disclosure, not a bug
report: neither has had upstream review, and each may be wrong about that project's intent.

| doc | where |
| --- | --- |
| [prover-cvc5-brokenpipe.md](prover-cvc5-brokenpipe.md) | The QED prover crashes on large SMT formulas. |
| [sqlparser-is-distinct-from.md](sqlparser-is-distinct-from.md) | `sqlparser` parses the right operand of `IS [NOT] DISTINCT FROM` at precedence 0, so it swallows a following `AND`. Our workaround is in `src/normalize.rs`. |
