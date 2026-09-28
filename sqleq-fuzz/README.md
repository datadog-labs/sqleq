# sqleq-fuzz

A **license-clean concrete differential tester** — the `fuzz` axis of `sqleq`, and the only one of
its three axes that can *refute*. It is a SQL non-equivalence disprover, and so also an independent
oracle/soundness check on the two proving axes: the [QED](https://github.com/qed-solver/prover)
prover and SQLSolver. The approach has already earned its keep: an earlier prototype of it found a
genuine soundness bug in the QED prover.

## What it does

For a query pair `(A, B)` under a schema, it repeatedly:

1. generates a small **valid** random database instance — honouring `NOT NULL` and *every* `UNIQUE` /
   `PRIMARY KEY` / `UNIQUE INDEX`;
2. binds `$N` parameters to random typed values, **consistently across A and B**, biasing each param
   toward a value that actually occurs in the column it is compared against (so equality filters
   match rows) — but only where that consistency is something the row supports, see
   [Parameter binding](#parameter-binding-is-an-assumption-not-a-given) below;
3. freezes `now()` / `current_*` to constants and skips truly nondeterministic functions;
4. runs both statements on **DuckDB** (fetched and linked by the build — nothing to install);
5. compares the outputs as **sorted multisets** (bag semantics — an `ORDER BY`-only difference never
   counts). `SELECT` compares the result set; `UPDATE`/`DELETE`/`INSERT` compares final table state.

Any difference on a valid, deterministic instance is a **sound counterexample** ⇒ the pair is
**non-equivalent**. This is a disprover: it can show non-equivalence (with a witness), never prove
equivalence.

## Soundness rules (a false positive is a bug)

A reported counterexample is only valid if the instance is valid *and* both queries are
deterministic. The hard-won rules, all preserved from the Python original:

- **Enforce every uniqueness constraint.** Missing one lets us fabricate an instance no valid
  database admits. `CREATE UNIQUE INDEX` that the parser drops is recovered by a regex fallback over
  the raw DDL; partial indexes are treated as *total* (conservative — only shrinks the valid space).
- **Freeze time.** `now()`/`statement_timestamp()` and the bare `current_timestamp`/`localtimestamp`
  keywords are frozen; otherwise A and B (run microseconds apart) disagree spuriously.
- **`LIMIT`/`OFFSET` over an unordered set.** Pure row-limit params are bound large (never truncate).
  A remaining literal limit, a dual-purpose param limit, or a string-flattening aggregate marks the
  pair *nondeterministic*, after which only **cardinality** differences (which stay deterministic)
  are trusted.
- **Canonicalize arrays.** `array_agg`/`unnest` element order is nondeterministic without `ORDER BY`,
  so list elements are sorted before comparison.
- **Don't invent a parameter correspondence.** See the next section.
- **Shim a Postgres function only where the mapping is exact.** DuckDB has no name for some of the
  functions these queries call, and both sides then fail to bind, so `src/shim.rs` supplies them as
  macros. Applying the same macro to both sides is not enough to make a loose mapping safe: if the
  two sides call the function on different arguments that Postgres maps to one value, a mapping
  that keeps them apart refutes an equivalent pair. Anything needing a real translation rather than
  a rename — format strings, regex semantics, full-text and jsonpath — is left undefined, and the
  pair keeps reporting an error.
- **A shim has to refuse what Postgres refuses.** Refusing an input is part of a function's
  semantics, and being more permissive is the unsafe direction: Postgres raises for
  `json_array_elements` of a non-array, where DuckDB answers with an empty list. An error makes the
  tester skip the trial, so the two sides are never compared; an empty answer instead drops a row,
  and a pair whose sides differ only in how they treat a dropped row is then refuted on an input
  Postgres would have rejected. So the set-returning shims check the type and raise.

The frontend faces the same question from the proving side, where the consequence is a false *proof*
rather than a false counterexample; `../docs/SOUNDNESS.md` is that argument.

## Parameter binding is an assumption, not a given

Substituting one value per `$N` puts the same value in A's `$N` and B's `$N`. That is **index
binding** — `$1` on the left is `$1` on the right because they share a number. What the caller means
is **intended binding**: `$1` on the left is whichever placeholder on the right the application fills
from the same value. A `(A, B, DDL)` row does not record the call site, and a rewrite that drops, adds
or reorders a placeholder renumbers everything after it.

Where the two queries mention **different sets of `$N`** and both are parameterized, index binding is
visibly not the caller's, and **neither verdict transfers**: a difference is a difference between two
queries the caller never paired, and an agreement is agreement about the wrong pair. Those report
`PARAM-MISALIGNED` with the evidence, instead of a claim. A pair that was already reporting `ERROR`
keeps it rather than being relabelled, because untestability is true whatever the numbering is and is
the half a reader can act on. Some of those errors are ones no binding could have produced — a name or
a type DuckDB does not have — and others are parameter-typing errors the identification may or may not
have provoked, since the crate's `VType::Integer` fallback produces the same message on well-aligned
pairs. Separating those two would need the counterfactual re-run `sqleq-frontend`'s
`params::root_cause_lowered` does, and it would change a label, never a claim.

**This test is deliberately stricter than the prover side's.** `sqleq-frontend`'s `params::check_arity`
runs the same comparison and exempts sets that differ *disjointly* — no shared index — because for a
**prover**, quantifying the two queries' parameters independently is a statement *stronger* than the
caller's, so proving it is sound and only ever costs completeness. For a **disprover** the same move is
fatal in the other direction: a witness anywhere in the wider space need not lie on the caller's
diagonal. `WHERE a = $1` against `WHERE a = $2` shares no index, and drawing `1` for `$1` and `2` for
`$2` fabricates a counterexample to an equivalent pair — which is what this crate did before the rule
landed (pinned by `disjoint_parameter_sets_are_misaligned_too`). So the condition here is *any* index
in one query and not the other. Real pairs have not forced it — every misalignment observed so far
overlaps and the disjoint class has been empty — so it costs nothing measurable and exists for the
argument, not the measurement.

### What the withdrawn claims actually were

Refusing costs verdicts, so a withdrawn claim is classified by what evidence the pair carries about
its own numbering, rather than left in one lump:

| what the pair shows | withdrawing the claim is |
| --- | --- |
| a shared `$N` provably compares against **disjoint columns** on the two sides | a **correction** — the claim was about the wrong pair |
| one side's indices have a **gap**, so that side was not renumbered | probably a cost — but see below, the exemption is unsound |
| **pagination-shaped**: every orphan is a `LIMIT`/`OFFSET` count and shared roles agree | probably a cost |
| no evidence either way | unknowable; refusing is the only sound move |

The corrections are the ones that justify the rule on their own. They include a pair a prover had
called equivalent and *this crate had corroborated* with `NO-COUNTEREXAMPLE` — neither axis could
clear it, because both were answering the wrong question — and a pair whose `$4` is a boolean on one
side and a row count on the other. Some of the corrections had been reporting `NOT-EQUIVALENT`.

Two things this test does **not** cover, both stated rather than papered over:

- A pure **permutation** — the same set on both sides with two indices swapped — is invisible to an
  *arity* test like this one, and index binding then compares the wrong diagonal with nothing to flag.
  This is the residual hole. It is not invisible to a *role* test: one observed pair has
  `first_name = $4 AND last_name = $5` against `last_name = $4 AND first_name = $5`, and had been
  reporting `NO-COUNTEREXAMPLE` because two `text` columns rarely separate under a swap. The analogue
  of the prover side's `check_roles`, built on per-query `param_cols` evidence, is the next step, and
  the corrections above are its measured yield.
- A genuinely benign renumbering loses its verdict, and that is a real share of the withdrawn claims.
  The obvious rescue is the **gap** test: a side whose indices skip a number cannot have been
  renumbered, since renumbering is contiguous. It is unsound, and the swapped pair above is why — its
  `B` set is `{2,…,6}`, a gap at `$1` from `SELECT $1` becoming `SELECT 1`, and its `$4`/`$5` are
  swapped anyway. A *leading* gap says nothing about the order of what follows. So the gap rows stay
  refused, and the other tempting refinement ("the orphans all sit above every shared index") is
  refuted by a real pair too.

## Usage

This crate is a workspace member but *not* a default one — its first build downloads DuckDB's
release library, so a bare `cargo build` at the workspace root skips it. Build it explicitly:

```
cargo build -p sqleq-fuzz --release     # first build downloads libduckdb (~40 MB) into target/
cargo test  -p sqleq-fuzz               # the self-contained suite below
```

```
sqleq-fuzz csv  <corpus.csv> <names.txt> [out.json]   # batch (rows are a,b,ddl); names are pairNNNN
sqleq-fuzz row  <corpus.csv> <index>                  # one corpus row, print the counterexample
sqleq-fuzz file <pair.sql>                            # CREATE TABLEs + exactly two statements

options: --jobs N (csv workers)  --trials N (default 120)  --rows N (default 5)  --seed N (default 0)
```

`csv` mode writes `{ "pairNNNN": { "verdict": "..." } }`, which is the output shape of the Python
tester it replaced. Verdicts: `NOT-EQUIVALENT`, `NO-COUNTEREXAMPLE`, `ERROR:...`,
`PARAM-MISALIGNED:...`, `NO-SCHEMA`, `NO-TABLES`, `NONDET-SKIP`. The two that carry a message after
a `:` still bucket correctly for a consumer that splits on the first one.

### The generated value domain (why a literal can make a pair look equivalent)

Column values are drawn from a deliberately small domain — `0,1,2` for integers, `'a','b','c'` for
strings, three dates and three timestamps — so that joins, `GROUP BY` and `DISTINCT` actually
collide on small instances. Parameters are additionally biased toward a value the column really
holds; a **literal is not**. A predicate against a literal outside the domain, `status = 'active'`,
is therefore satisfied by no generated row: both sides return nothing on every trial and a
non-equivalent pair reports `NO-COUNTEREXAMPLE`.

This bites `file` mode hardest, since a hand-written pair carries literals where a corpus row
carries `$N`. Write self-contained pairs against the generated domain — the committed
[`examples/`](../examples) do, which is why they refute.

## Validation

This crate is a port of an earlier Python tester, and it was cross-checked against that tester on a
sample of pairs under identical defaults before it replaced it:

- **Identical `NOT-EQUIVALENT` set**, `UPDATE` DML included — **zero new false positives, zero power
  regressions**. That set is the one to check: a port that refutes a pair the original did not is
  exactly the failure this comparison exists to catch.
- Class agreement everywhere else was near-total, and every disagreement was the Rust port being
  *more* capable — schemas sqlglot rejected and it parses, pairs the Python tester errored on and it
  runs. All of them resolve to a safe `NO-COUNTEREXAMPLE` or `ERROR`, never a spurious
  `NOT-EQUIVALENT`.

`cargo test` runs a self-contained suite (no corpus) covering the bag-semantics, uniqueness-recovery,
time-freezing, and non-equivalence-detection rules.

## Notes

- Clean-room port; shares no code with the NonCommercial VeriEQL.
- DuckDB is linked from its own release library (`duckdb` crate, MIT), which the build downloads
  into `target/` and copies next to the executable; parsing uses `sqlparser` (the same parser as
  `sqleq-frontend`); `regex`, `rand`, `csv` complete the dependency set — all permissive licenses.
