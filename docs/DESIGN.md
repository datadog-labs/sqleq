# Design decisions behind the Rust frontend

**Status:** history, kept for the reasoning.

A premise and four decisions shaped `sqleq-frontend`, and this is the only place the *arguments* for
them are written down. That is the whole reason this file still exists. It does **not** describe the
repo as it is — for that read [../README.md](../README.md), and for the running record
the engineering log, which is not published.

It is what survives of the original proposal, *A Rust Frontend for QED (retiring Java/Calcite +
Python)*, trimmed on 2026-09-22; `git show 4295db5:DESIGN.md` is the full text. What was cut had
stopped carrying information: a staged plan that is finished (the Java parser retired in `c47accf`,
the Python preprocessor not long after), effort estimates spent, risks resolved, a "recommended next
action" long since taken, and a coverage baseline several corpus revisions stale. The scope was
narrower than what shipped, too — the proposal was a frontend for one prover, and `sqleq` now runs
three axes over it: `qed` and `sqlsolver` (both through this IR) and `fuzz` (which reads no IR at
all).

---

## 1. Parsing is the easy 20%; the analyzer is the 80%

This is the premise the parser choice rests on. The Java parser did four jobs, and only the first
was "parsing":

1. **Lex + parse** → syntax tree.
2. **Name/scope resolution** — bind `t.col` to a source; aliases; nested & correlated subqueries.
3. **Type inference / coercion.**
4. **Lower to relational algebra with positional indices** — the IR references columns by a
   **variable level (de-Bruijn-style index into the flattened scope)**, not by name. This is the
   `Env`-offset bookkeeping that Calcite's `RexInputRef` + the qed-parser produced.

Steps 2–4 — *the analyzer* — are the real work, and they are identical regardless of how we parse.

## 2. Parser: sqlparser-rs, not DataFusion, not ANTLR

| Option | Parsing | Analyzer (steps 2–4) | Call |
|---|---|---|---|
| **sqlparser-rs + our analyzer** | mature typed Rust AST, Postgres dialect | we write it (port the sqlglot resolver + Env indexing) | **chosen** |
| Apache DataFusion | sqlparser-rs | reuse its `LogicalPlan`, walk → Relation | fallback accelerator; planner is opinionated (folds/reorders) |
| ANTLR pg grammar → Rust | best raw *syntax* coverage | **still entirely on us**, over a CST | rejected as primary |

**Why not ANTLR** — the original idea: it only solves step 1, the easy part, and returns a CST
clunkier than sqlparser-rs's typed AST, on a less-mature Rust runtime. Its advantage is broad
Postgres *syntax* coverage, but **coverage was never the bottleneck** (we already reject exotic
syntax); the analyzer is. Worth keeping the grammar as a *reference* if sqlparser-rs shows a
specific gap.

**Why not DataFusion:** we want a *faithful, predictable* lowering, and DataFusion's planner
rewrites and reorders plans, which fights the need for structural fidelity. The conceptually-hard
resolver logic also already existed in Python, so the port was "translate + harden," not "design
fresh."

Both were left open as fallbacks pending a calibration spike, on the theory that a hand-written
analyzer might prove too costly. The calibration spike closed the question: a Rust analyzer of a
few hundred lines reproduced Calcite's resolution, typing and de-Bruijn indexing exactly, including
joins. Neither fallback was needed.

## 3. The Java oracle compares verdicts, not IR

The transition was de-risked by keeping the Java parser as an **oracle**: run both frontends over
every pair in the corpora and over the prover's Calcite test suite, diff, and treat any divergence
as a frontend bug. That converts "reimplement a SQL frontend" into measurable convergence against a
trusted reference on a large, realistic test set, and retiring Java was gated on a clean diff.

**What the diff compares is the actual decision.** The spike's finding was to compare **verdicts,
not raw IR**. Calcite applies semantics-preserving optimizations — column pruning, trivial-node
elimination — so raw-IR diffing flags spurious differences. The robust oracle is therefore to
require the *prover* to agree (both provable, or both not), with raw-IR equality as a stricter,
optional bonus check once the frontend implements pruning of its own.

The oracle has since served its purpose and is closed; v34 records why its parity number is final
rather than stale.

## 4. Normalizations: "sound" is separate from "coping"

The preprocessor's passes were a mix of the two. The decision was to keep them as discrete AST
passes in the port, and to split them by which kind they are:

- **Sound, general rewrites** (always on): DISTINCT-in-IN strip, DELETE/UPDATE→SELECT reduction,
  identical-pagination strip, CTE inlining, ANY(ARRAY) desugar, builtins/operators →
  uninterpreted.
- **Corpus-coping heuristics** (configurable, default-on for messy input, off for clean catalogs):
  opaque sort for untyped columns, type-confidence inference, schema synthesis when DDL is absent.

The split is what makes it possible to say which rewrites the soundness argument depends on, and to
turn the rest off for a clean catalog.

## 5. The hard rule: refuse, never emit best-effort IR

**If the frontend cannot lower a construct faithfully, it must ERROR and emit nothing.** Surfaced by
the spike, where a best-effort cut silently mis-lowered a `GROUP BY` — harmless in that instance,
but exactly the path to a future false positive.

Unlike the decisions above, this one *is* recorded elsewhere, because it has to be: it is enforced
at the boundary in [`../src/lib.rs`](../src/lib.rs) and restated in the README. It is written down
here because this is where it came from.
