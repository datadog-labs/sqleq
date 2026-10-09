# sqleq-fuzz

A **license-clean concrete differential tester** — the `fuzz` axis of `sqleq`, and the only axis
that can *refute*. It is a SQL non-equivalence disprover, and so also an independent
oracle/soundness check on the axes that prove under the same parameter binding it tests: the
[QED](https://github.com/qed-solver/prover) prover, `sqleq-solver` (a Rust rewrite of SQLSolver)
and the JVM SQLSolver kept as its cross-check. The approach has already earned its keep: an earlier
prototype of it found a genuine soundness bug in the QED prover.

It is normally run through [`sqleq-check`](../sqleq-check/README.md), as `--axes fuzz` or beside
the provers under `--portfolio`; the [Usage](#usage) below is for running it on its own.

## What it does

For a query pair `(A, B)` under a schema, it repeatedly:

1. generates a small **valid** random database instance — honouring `NOT NULL` and *every* `UNIQUE` /
   `PRIMARY KEY` / `UNIQUE INDEX`, wherever the DDL states it — with every table at full size in most
   trials and empty or nearly so in the rest;
2. binds `$N` parameters to random typed values, **consistently across A and B**, biasing each param
   toward a value that actually occurs in the column it is compared against (so equality filters
   match rows) — but only where that consistency is something the row supports, see
   [Parameter binding](#parameter-binding-is-an-assumption-not-a-given) below;
3. sets the clock (`now()`, `current_*`) to one instant per trial and skips truly nondeterministic
   functions;
4. runs both statements on **PostgreSQL 17**, in a private cluster it starts for the run (see
   [Engines](#engines)) — or, with `--engine duckdb`, on DuckDB, as DuckDB evaluates them;
5. compares the outputs as **sorted multisets** (bag semantics — an `ORDER BY`-only difference never
   counts). `SELECT` compares the result set; `UPDATE`/`DELETE`/`INSERT` compares final table state,
   and the returned rows as well when both sides carry `RETURNING`. A pair with no one observable
   to compare — a query against a mutation, `RETURNING` on one side only, or an `EXPLAIN` — is
   reported `NOT-COMPARABLE` instead of run, and so is a pair the engine cannot evaluate faithfully
   (see [Engines](#engines) and the rules below).

Any difference on a valid, deterministic instance is a **sound counterexample** ⇒ the pair is
**non-equivalent**. This is a disprover: it can show non-equivalence (with a witness), never prove
equivalence.

## Engines

`--engine postgres`, the default, runs both statements on PostgreSQL itself, so there is nothing
to emulate: what Postgres computes is the answer. `--engine duckdb` runs them on DuckDB instead, as
DuckDB evaluates them: nothing makes DuckDB compute what Postgres computes, so a DuckDB
counterexample shows the two statements differ under DuckDB, not that they differ under Postgres
(DuckDB binds `->>` looser than `AND`, reads a bare `float` as a 4-byte `REAL`, divides integers as
floats, and has no name for many Postgres functions). It also builds its tables from its own reading
of the DDL, which enforces `NOT NULL`, primary keys and unique constraints but not `CHECK` or
`FOREIGN KEY` constraints. Its output always says `engine: duckdb`, and `sqleq-check` runs the
Postgres engine only.
The Postgres engine starts a private PostgreSQL 17 cluster for the run — from `$SQLEQ_PG_BIN`, else
the PostgreSQL the build fetched (a digest-pinned prebuilt PostgreSQL 17 for Linux and macOS on
x86_64 and arm64, unless `SQLEQ_PG_DOWNLOAD=0`) if it runs there, else the `postgres` on `PATH`; any
other major version is refused — serves it on a unix socket in a fresh
temp directory, and stops and removes it when the run ends (or when the process is killed). The
first run builds a template cluster under `$SQLEQ_PG_CACHE` (default `~/.cache/sqleq`); every later
one copies it. `$SQLEQ_FUZZ_ENGINE` sets the default, which is how `sqleq-check` picks the engine.

How the Postgres engine differs from the DuckDB one:

- **The DDL runs as written**, once per pair, inside a transaction rolled back at the end, so every
  constraint it declares — `CHECK` and `FOREIGN KEY` included, which the DuckDB engine does not
  read — is Postgres's to enforce, and a generated row Postgres refuses is not in the instance.
  Captured DDL is made to run only in ways that add no constraint: a schema it names is created, an
  unqualified table that the rest of the DDL names by one schema is created there (a schema in which
  the DDL creates a table of that name itself does not count: an unqualified `t` beside an `s.t` is
  `public.t`), a table the queries name by one other schema is moved there, a type nothing declares
  is read as `text`, and a column default that calls a function nothing declares is dropped. The
  last two are reported as a `caveat`, since a verdict then rests on more than the DDL says.
- **Each `$N` is typed as Postgres types it.** Both statements are prepared, and each placeholder
  gets the type Postgres infers for it — with the column the heuristics link it to declared as a
  hint where Postgres cannot infer one — and one type across the pair: a side that leaves `$N`
  untyped, or that Postgres merely reads as `text` (`SELECT $1 AS x`), takes the other side's type;
  a side that casts it keeps its cast. A `$N` that is an array on one side and a scalar on the
  other is `NOT-COMPARABLE`. Values are still written into the statement, cast to that type, rather
  than bound as parameters: captured SQL writes a `$N` where no parameter may stand
  (`interval $1`).
- **Rows are compared under Postgres `=`.** Bags whose text is the same are the same; two bags of
  one size whose text differs are compared again in Postgres, so `1.0` and `1.00` are one value
  while their text is not. A column whose type has no `=` (`json`, `xml`, `point`, ...) is compared
  by its text, since no two such values are one under `=`: `true` and `1` are two `json` values.
- **The clock is an input of each trial.** Every read of it, in both statements and in the DDL's
  column defaults, reads one instant the trial sets: in turn, one before every generated date and
  timestamp (1970), one among them (a generated timestamp, read off the generator's domain), and
  one after them (2100). Against the first and last, a comparison with the clock holds for every
  generated row or for none, so a side that drops one is seen; against the middle one, `<` and
  `<=` differ. A counterexample names its clock. Each sequence is reset before each side that can
  write.

## Soundness rules (a false positive is a bug)

A reported counterexample is only valid if the instance is valid *and* both queries are
deterministic, on either engine. The rules:

- **Enforce every uniqueness constraint.** Missing one lets us fabricate an instance no valid
  database admits. Constraints are read inline, as table constraints (a table-level `PRIMARY KEY`
  makes its columns `NOT NULL` too), from `CREATE UNIQUE INDEX`, and from `ALTER TABLE … ADD
  PRIMARY KEY | UNIQUE` and `ALTER COLUMN … SET NOT NULL`. A `serial` column (`smallserial`,
  `bigserial`, `serial2/4/8`) and an identity column (`GENERATED … AS IDENTITY`) are `NOT NULL`,
  and draw distinct values, as a fresh sequence does (that is not a key: Postgres lets an explicit
  value repeat one). A unique index over an expression is
  created as a DuckDB unique index on that expression (or, where DuckDB will not index it, checked
  after each insert), and `NULLS NOT DISTINCT` admits at most one NULL key. `CREATE UNIQUE INDEX` that the parser drops is recovered by a regex fallback over the raw
  DDL; partial indexes are treated as *total* (conservative — only shrinks the valid space). A
  uniqueness statement nothing can read withholds every pair over its table (`NOT-COMPARABLE`).
- **A table is the table Postgres resolves.** `public.t` and `t` are one table (the default
  `search_path`), so `public.` is dropped from the queries. Tables of one name in two schemas
  (`s1.t`, `s2.t`) draw rows of their own. Any other second spelling of a table in a mutation pair
  (`s.t` beside a DDL's `t`) is withheld, since a write through one spelling would not show
  through the other.
- **One clock for everything a trial reads.** `now()`/`statement_timestamp()`/
  `transaction_timestamp()` and the bare `current_timestamp`/`localtimestamp` keywords, and
  `current_time`/`localtime`/`current_date`, all read one instant, so `now()::time = localtime`
  holds as in Postgres; otherwise A and B (run microseconds apart) disagree spuriously. A clock
  spelled inside a string literal or a comment is left alone. On Postgres the instant is the
  trial's (see [Engines](#engines)), and it is read by the DDL's defaults too, an old dump's
  `('now'::text)::date` included, so an insert that writes `now()` and one that leaves it to the
  default agree. A function body that reads the clock (a trigger stamping a row) would read the
  real one, so a pair that reads the clock is withheld when one does, and so is one whose stored
  default still reads the real clock. The DuckDB engine freezes the clock at 2020-06-01 12:00:00
  UTC.
- **`LIMIT`/`OFFSET` over an unordered set.** The clauses are read off the parse, so `LIMIT (1)`,
  `FETCH FIRST ROW ONLY` and `LIMIT ($1)` count. A count that is a bare `$N` and nothing else is
  bound so that it cuts nothing: a `LIMIT` large, an `OFFSET` to 0 — except in half the
  small-instance trials, where it is bound so that it does cut (and those trials compare only
  cardinality), so a cut on one side alone is still seen. Any other cut, and a
  string-flattening aggregate, marks the pair *nondeterministic*, after which only **cardinality**
  differences (which stay deterministic) are trusted — unless the cut's `ORDER BY` is provably a
  total order (it determines a row of every table through a NOT NULL key, the join's equalities and
  the columns `WHERE` pins), in which case the rows it keeps are determined and are compared whole.
  The cardinality stays deterministic only while nothing above the cut can tell which tied rows it
  kept. Postgres keeps the tied rows that come first in the order it reads them, so two equivalent
  sides that read the same rows in different orders (through a join, a `DISTINCT`, a `UNION ALL`)
  keep different ones, and a `WHERE` above lets a different number of them through. So a cut whose
  order is not total is compared by cardinality only where how many rows it keeps fixes how many the
  result has, whichever rows they are: every level above it is parentheses, a `UNION ALL` branch,
  an `ORDER BY`, or a `SELECT` that reads the cut's rows only in its select list (no `WHERE`, join
  condition or `LATERAL` entry that reads them, no `GROUP BY`, `HAVING`, `DISTINCT` or
  set-returning function), up to the statement itself, an `INSERT`'s source with no `ON CONFLICT`,
  or an `EXISTS`, which reads only whether there is a row. Anywhere else (under a filter or a join
  on its rows, a grouping or a slice, in an `IN` or scalar subquery, a CTE, an `UPDATE` or a
  `DELETE`) it makes the pair `NONDET-SKIP`; a bare `$N` count there is compared only on the trials
  that bind it so that it cuts nothing.
- **Ties in `DISTINCT ON` and in a window `ORDER BY`.** These choose among tied rows too: the row a
  `DISTINCT ON` keeps per key, and the order an order-sensitive window function (`row_number`,
  `lag`, `first_value`, a `ROWS` frame, ...) numbers or reads them in. Postgres chooses by physical
  order, so it can choose differently on the two sides of an equivalent pair. A choice is compared
  whole when the tied rows cannot be told apart in the result -- its order is total, or every
  column the level's select list reads is one the tied rows agree on. Otherwise a `DISTINCT ON` at
  the top level (or in a `UNION ALL` branch of it) is compared by cardinality, which is its number
  of keys, and anything else makes the pair `NONDET-SKIP`.
- **Canonicalize arrays.** `array_agg`/`unnest` element order is nondeterministic without `ORDER BY`,
  so list elements are sorted before comparison.
- **Compare numbers by value, not by type.** A declared `bigint` is materialized as DuckDB `INTEGER`,
  so `c` and `c::bigint` come back as different DuckDB types carrying the same number, and a
  `numeric` of another scale does the same. Cells are compared by numeric value, which can only merge
  them, never split them — inside a record, a map or an array as well, where a record's field names
  are not compared either. An interval is compared by its span (a month as 30 days, a day as 24
  hours), as `=` compares intervals in DuckDB and in Postgres, so `'1 day'` and `'24 hours'` are one
  cell.
- **Read placeholders off the tokens.** `$N` is found by the tokenizer, never inside a string literal
  or a comment, and substituted at those positions only. A placeholder Postgres would reject (`$0`,
  a number past `u32`) makes the pair `ERROR`.
- **Don't invent a parameter correspondence.** See the next section.

The frontend faces the same question from the proving side, where the consequence is a false *proof*
rather than a false counterexample; [`docs/SOUNDNESS.md`](../docs/SOUNDNESS.md) is that argument.

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
in one query and not the other. It exists for the argument, not for any measured yield.

### What the withdrawn claims actually were

Refusing costs verdicts, so a withdrawn claim is classified by what evidence the pair carries about
its own numbering, rather than left in one lump:

| what the pair shows | withdrawing the claim is |
| --- | --- |
| a shared `$N` provably compares against **disjoint columns** on the two sides | a **correction** — the claim was about the wrong pair |
| one side's indices have a **gap**, so that side was not renumbered | probably a cost — but exempting it would be unsound, see below |
| **pagination-shaped**: every orphan is a `LIMIT`/`OFFSET` count and shared roles agree | probably a cost |
| no evidence either way | unknowable; refusing is the only sound move |

The corrections are the ones that justify the rule on their own. A shared `$N` that compares
against disjoint columns on the two sides — or that is a boolean on one side and a row count on the
other — was never one pair to begin with, so a prover's proof of it and this crate's
`NO-COUNTEREXAMPLE` beside it would both answer the wrong question, and so would a
`NOT-EQUIVALENT`.

Two things this test does **not** cover, both stated rather than papered over:

- A pure **permutation** — the same set on both sides with two indices swapped — is invisible to an
  *arity* test like this one, and index binding then compares the wrong diagonal with nothing to flag.
  This is the residual hole. It is not invisible to a *role* test: a pair such as
  `first_name = $4 AND last_name = $5` against `last_name = $4 AND first_name = $5` can report
  `NO-COUNTEREXAMPLE`, because two `text` columns rarely separate under a swap. The analogue of the
  prover side's `check_roles`, built on per-query `param_cols` evidence, is the next step.
- A genuinely benign renumbering loses its verdict, and that is a real share of the withdrawn claims.
  The obvious rescue is the **gap** test: a side whose indices skip a number cannot have been
  renumbered, since renumbering is contiguous. It is unsound, because a pair can carry a gap and a
  swap at once: `SELECT $1` rewritten as `SELECT 1` leaves `B`'s set `{2,…,6}`, gapped at `$1`, with
  its `$4`/`$5` swapped all the same. A *leading* gap says nothing about the order of what follows.
  So the gap rows stay refused, and the other tempting refinement ("the orphans all sit above every
  shared index") falls to the same kind of pair.

## Usage

This crate is a workspace member but *not* a default one — its first build downloads DuckDB's
release library, so a bare `cargo build` at the workspace root skips it. Build it explicitly:

```
cargo build -p sqleq-fuzz --release     # first build downloads libduckdb (~40 MB)
cargo test  -p sqleq-fuzz               # the self-contained suite below
```

Running pairs needs a PostgreSQL 17, found as [Engines](#engines) says — the build fetches one for
the common platforms; `--engine duckdb` needs none.

`sqleq-check` passes the trial budget explicitly (`--trials 120 --rows 5 --seed 0`), so a change to
the defaults below cannot move its answers. On its own:

```
sqleq-fuzz csv  <corpus.csv> <names.txt> [out.json]   # batch (rows are a,b,ddl); names are pairNNNN
sqleq-fuzz row  <corpus.csv> <index>                  # one corpus row (counting from 0)
sqleq-fuzz file <pair.sql>                            # DDL (CREATE, ALTER) + exactly two statements

options: -j/--jobs N (csv workers, default 1)  --trials N (default 120)  --rows N (default 5)
         --seed N (default 0)  --engine postgres|duckdb (default $SQLEQ_FUZZ_ENGINE, else postgres)
```

`row` and `file` print the verdict on the first line. A `NOT-EQUIVALENT` is followed by a
`counterexample: …` line holding the instance, and a `NO-COUNTEREXAMPLE` that only some trials
reached by a `partial: K trials compared both sides; last error: …` line. Those lines are what
`sqleq-check` reads. An `engine: postgres 17.N` (or `engine: duckdb …`) line follows, and with the
Postgres engine a `caveat: …` line when the verdict rests on a stood-in type or a dropped default.

`csv` mode takes each name in `names.txt` to the corpus row its digits number (`pairNNNN` is row
`NNNN`), prints a `name: LABEL Tms` line per row as it finishes (with `(ok=K/N)` after the label when
only some trials compared both sides), and writes `{ "pairNNNN": { "verdict": "...", "ms": ... } }`
to `out.json` (default: beside the corpus, its extension replaced by `.fuzz.json`), adding
`ok_trials` and `trial_error` for a partial run, `engine`, and with the Postgres engine, where there
is one, `caveat`. A name whose digits number no row gets `NO-ROW`. A
panic while testing one row is that row's `ERROR:panic: …`, and the run goes on to the next one.

Verdicts: `NOT-EQUIVALENT`, `NO-COUNTEREXAMPLE`, `ERROR:...`, `PARAM-MISALIGNED:...`,
`NOT-COMPARABLE:...`, `NO-SCHEMA`, `NO-TABLES`, `NONDET-SKIP`. The three that carry a message after
a `:` still bucket correctly for a consumer that splits on the first one. `NOT-COMPARABLE` is a
withheld verdict: the two sides share no observable, a `$N` is an array on one side and a scalar on
the other, a pair needs an ICU collation, or the generated instances cannot stand for the DDL.

The exit code is `0` whatever the verdict, `NOT-EQUIVALENT` included; `1` when the input cannot be
read or an argument is missing (a missing file, a row out of range, a file without exactly two
statements besides its DDL, `row` without an index), or when a `csv` worker died and left rows
without a verdict; `2` when the mode is missing or unknown, after printing the usage.

### The generated value domain (why a literal can make a pair look equivalent)

Column values are drawn from a deliberately small domain — `0,1,2` for integers, doubles and
numerics, `'a','b','c'` for strings, `true`/`false`, three dates, three timestamps, three UUIDs and
a few small JSON documents — so that joins, `GROUP BY` and `DISTINCT` actually collide on small
instances. Every table gets `--rows` rows in each of `--trials` trials; another `--trials / 4`
trials, interleaved with those and drawn separately, give each table between 0 and `--rows` rows,
mostly 0 or 1, which is where an aggregate over no rows, `EXISTS` or a scalar subquery tells two
queries apart.
Parameters are additionally biased toward a value the column really holds; a **literal is not**. A
predicate against a literal outside the domain, `status = 'active'`, is therefore satisfied by no
generated row: both sides return nothing on every trial and a non-equivalent pair reports
`NO-COUNTEREXAMPLE`.

This bites `file` mode hardest, since a hand-written pair carries literals where a corpus row
carries `$N`. Write self-contained pairs against the generated domain — the committed
[`examples/`](../examples) do, which is why they refute.

## Validation

`cargo test` runs a self-contained suite (no corpus) covering the bag-semantics, uniqueness-recovery,
time-freezing, and non-equivalence-detection rules. Its answer on every pinned pair is pinned as
well, and CI checks those pins with `sqleq-check --expect pinned --axes fuzz`
([`tests/pairs/`](../tests/pairs/README.md)). How a counterexample is played against the provers'
proofs, and what that cross-check has caught, is in [`docs/VALIDATION.md`](../docs/VALIDATION.md).

## Notes

- Clean-room; shares no code with the NonCommercial VeriEQL.
- DuckDB is linked from its own release library (`duckdb` crate, MIT). The build downloads it once
  into `target/duckdb-download/`, or links the one `DUCKDB_LIB_DIR` names; `libduckdb-sys` copies it
  into `target/<profile>/deps`, and the binary finds it there through an rpath relative to itself,
  so a built tree can be moved. Parsing uses `sqlparser` (the same parser as `sqleq-frontend`);
  `regex`, `rand`, `csv` and `serde_json` complete the dependency set — all permissive licenses.
