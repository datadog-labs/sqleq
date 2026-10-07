# SQLSolver: `sqleq-solver`, with the JVM fork as a cross-check

[SQLSolver](https://github.com/SJTU-IPADS/SQLSolver) (SIGMOD 2024, Apache 2.0) is a SQL equivalence
prover built independently of QED. **`sqleq-solver`** is a Rust rewrite of its proof engine
([below](#sqleq-solver)) and the axis of the same name: it reads the same `Input` JSON the QED
prover gets, needs no JVM, and is what `sqleq-check --sqleq-solver` asks. The original, run as a JVM
fork through `tools/sqlsolver/`, is kept as a **backup cross-check** and is being deprecated
([below](#the-jvm-fork)): `--sqlsolver-jvm` asks it instead, as the axis `sqlsolver-jvm`. A run asks
one of the two, so comparing them takes two runs over the same pairs.

Outside `--portfolio`, either one is a **second opinion**: its answer is reported beside QED's, and
neither a case's status nor the exit code of `--expect equivalent` or `report-only` depends on it,
except that its proof against a `sqleq-fuzz` counterexample on the same pair is an alarm, which
fails every run. Two modes do count it. Under `--portfolio`, a `sqleq-solver` proof is evidence like any other: it
can make a case `equivalent`, and the combined verdict decides the exit code. Under `--expect
pinned`, its pin is checked like every axis's, so a moved answer fails the run. The point of having
it is that provers of different construction, reading the same IR, check each other — see
[VALIDATION.md](VALIDATION.md).

The fork is the backup rather than the primary SQLSolver axis because it proves some pairs that
Postgres does not treat as equivalent, and because it cannot be rebuilt from public sources. The false proofs known so
far, and what stands between each and a reported verdict:

* **Any two `UPDATE`s of one table are `EQ`**, on the fork's SQL-text entry point
  ([below](#the-update-unsoundness-on-the-sql-text-path)). The bridge never takes that path: the
  frontend reduces a DML pair to the queries computing its effect before it lowers anything, so the
  fork is only ever handed two queries, and the bridge's plan entry skips the check where the
  collapse happens.
* **`UNIQUE` is read as "no duplicate rows at all"**, while Postgres allows any number of NULLs
  ([below](#unique-means-something-different-here)). The schema declares `UNIQUE` only over `NOT
  NULL` columns.
* **Every `CAST` is erased**, which equates `CAST(a AS REAL) / b` with `a / b`. The conversions the
  frontend names as functions (temporal conversions, qualified and opaque targets) survive as such;
  a plain `CAST` does not, and nothing guards it.
* **A boolean value in a `SELECT` list is read as two-valued.** The fork proves `SELECT x IS NOT
  NULL AND x <= 5 FROM t` equivalent to `SELECT x <= 5 FROM t`, but on a row where `x` is NULL the
  first is FALSE and the second is NULL. In a `WHERE` the two filter alike, so only a projected
  value is affected. Nothing guards this either.

`sqleq-solver` gets each of these right by construction (see [*Where it deliberately
differs*](#where-it-deliberately-differs-from-the-fork)). So an `EQ` from the fork that
`sqleq-solver` does not share is a prompt to look, not a proof; and an `EQ` from either is a claim
to check against `sqleq-fuzz`.

## Reproducing this

`sqleq-solver` is reproducible from this repository alone, Z3 included: `cargo build --release -p
sqleq-solver`, and `--sqleq-solver` in `sqleq-check` finds it. The JVM fork is **not reproducible
from this repository alone.** `sqleq-check --sqlsolver-jvm` needs `$SQLEQ_SQLSOLVER` (or
`--sqlsolver-tree DIR`), a hand-modified fork with Calcite removed, its classes compiled into
`build/classes-javac`; `$SQLEQ_SQLSOLVER_DEPS`, the upstream fat jar exploded with
`org/apache/calcite` removed; and a JDK on `PATH`. **That fork is not published.** The sections
below state what the fork has to do, which is enough to redo the work, but redoing it is a rebuild
rather than a checkout.

The Java sources that *are* in this repository — `tools/sqlsolver/` — are our side of the bridge.
`sqleq-check` compiles them against the fork with the system `javac`, into
`tools/sqlsolver/out-fork/`, whenever they are newer than the classes there; they do nothing
without the fork.

## Verdicts are not symmetric

Both drivers answer each row with one raw verdict, and `sqleq-check` buckets it
(`sqleq-check/src/axes/solver.rs`). Only `EQ` is a claim:

| raw | means | bucket |
|---|---|---|
| `EQ` | proved equivalent | `proved` / `proved-literal` |
| `NEQ` | **no proof found** | `no-proof` |
| `UNKNOWN` | no proof found | `no-proof` |
| `NOIR`, `NOTRANS` | the frontend built no plan, or the plan could not be translated — ours, not theirs | `unsupported` |
| `TIMEOUT`, `HANG` | the per-row cap ran out; `HANG` is a worker that ignored its interrupt | `timeout` |
| `ERROR` | the driver failed on the row, or died on it | `error` |
| — | no answer came back for the row | `missing` |

`EQ`, `NEQ`, `UNKNOWN` and `TIMEOUT` are SQLSolver's own `VerificationResult` values, and
`sqleq-solver` never writes `TIMEOUT`. A row the cap cut off is marked `killed`, whatever it then
answered, and that turns anything but an `EQ` into `timeout`: "we stopped asking" is not "they found
no proof". After a `HANG` the driver halts its own process, and `sqleq-check` resumes it on the rows
still unanswered. A driver killed on a row — out of memory under a cap, or a crash — has that row
recorded as `ERROR` with `died: true`, and the pass goes on past it.

**`NEQ` is not a counterexample.** SQLSolver is meant to be sound and is incomplete, and `NEQ` is
what it returns when a proof attempt fails, not when it has refuted anything. The harness collapses
`NEQ` and `UNKNOWN` into one bucket deliberately, so that no downstream reader can read `NEQ` as a
refutation. `sqleq-fuzz` remains the only disprover in the system.

### Two kinds of `EQ`

An `EQ` can come from a proof or from the two sides being one plan already, and the two are not
worth the same. On its SQL-text entry point the fork answers the second kind first: before it solves
anything, `getVerifyResult` asks `PlanSupport.isLiteralEq`, which compares the plan trees assembled
from the two texts. An `EQ` from there says the two queries are the same query — not that the prover
related two different ones. Counting those as capability repeats the mistake the qed axis's own
reflexivity check exists to avoid.

The bridge's plan entry skips `isLiteralEq`, so both drivers ask that question themselves, as a
tier 0 before any solving: `sqleq-solver` and `IrDriver` alike compare the two raw IR trees and
label an `EQ` from there `"literal": true`. `proved` means the solver proved it; `proved-literal`
means the two sides were identical and no solving happened. An `EQ` with no label reads as a proof
rather than being quietly downgraded, so the inflation this guards against cannot come back as an
undercount.

## The second opinion in `sqleq-check`

`sqleq-check --sqleq-solver` asks `sqleq-solver` about every pair in a run, or
`--sqlsolver-jvm` the fork, and reports the answer beside the qed one. Three design constraints
shaped it, all from the fork, and the pass keeps them for both so that the two implementations'
answers stay comparable:

1. **One pass, at the end, by one driver process.** A JVM start is a large fraction of what a
   `.sql` pair costs, so paying it per case would swamp the run. `--sqleq-solver-jobs N` runs N
   drivers side by side, each over its share of the rows, at the price of the third constraint.
2. **The JVM self-halts** rather than unwinding, so something has to notice the missing answers and
   resume. A per-case call has nowhere to put that loop.
3. **The per-row cap is load-sensitive.** Under `-j 8` a row near the cap decides differently than
   it does alone, and a second opinion that changes with `-j` is not a second opinion.

So `run_case` does one SQLSolver-axis thing — it packages the plan, while the case's working
directory still exists — and a single pass at the end asks the driver about every job.
The packaging cost is discounted from the case wall time, so a second-opinion run's timings stay
comparable to one without it.

`sqleq-check --portfolio` is the one exception, and gives up the third constraint on purpose: it
asks `sqleq-solver` about each case beside the other backends, inside that case's deadline, because
one combined verdict within a time budget is what it is for. It never runs the JVM fork, which the
first two constraints are about. Its answers near the cap can therefore vary with load; the
sequential pass stays the one to compare the two implementations in, or to pin.

## `sqleq-solver`

`sqleq-solver/` is a Rust rewrite of the part of SQLSolver that the IR bridge reaches: from the
`Input` JSON to a verdict, without Calcite, without SQL text, and without a JVM. Its binary takes
`IrDriver`'s arguments and writes `IrDriver`'s rows, so `sqleq-check` drives either
implementation with the same buckets, the same resume loop and the same report.

What it rewrites, in the order the ladder runs it:

* **Tier 0** on the raw IR: two identical trees answer `EQ` with `literal: true`, before anything
  is parsed, exactly as `IrDriver` does.
* **Translation** to U-expressions (`UExprConcreteTranslator`), then **normalization**
  (`UNormalization`, `QueryUExprNormalizer`), **integrity-constraint rewriting**
  (`QueryUExprICRewriter`, with the constraints read from the IR's own schemas) and
  **alpha-equivalence** — the rung that answers most of SQLSolver's proofs.
  Normalization also merges a row's matched and unmatched summands (`Σ X·N + Σ X·¬N` is `Σ X`
  when `N` is 0/1, and has its zero-ness under a squash), which the fork reaches only through its
  LIA\* rung.
* **The set solver** (`SetSolver`), asking Z3 about terms whose every summation is under a squash
  or negation. Its values are one uninterpreted sort, not the fork's integers, reals and strings,
  so the only order fact it is given is that `a <= b` is `NOT (b < a)`. That holds wherever it is
  used: every order comparison sits under its operands' not-null guard, and the non-`NULL` values
  of one type are totally ordered.

Not rewritten: the **LIA\* rung**. What it adds over the rungs above is mostly reasoning across
summands (a disjoint `OR` against a `UNION ALL`, a count compared with a constant), and several of
its encodings do not hold for Postgres as written. Its integer reading of dates and timestamps is
one: `ts >= k AND ts < k + 1` is not `ts = k` for a timestamp, and `'infinity'::date + 1` is
`infinity`, so even `d + 1 > d` fails. The IR names every temporal operation and conversion
(`q_arith_add_date_integer`, `q_conv_date_timestamp` and the like), which is what such a rung would
have to interpret, infinities included. **`LIMIT`/`OFFSET`** (`OrderbySupport`) is not rewritten
either; a bare `ORDER BY` is erased, which is sound under the bag semantics `sqleq` decides.

### Where it deliberately differs from the fork

Each difference is there for soundness:

* **Three-valued logic is explicit.** A predicate translates to separate TRUE and FALSE terms, so
  `NOT` of an UNKNOWN comparison stays UNKNOWN and `NOT IN` gets its `NULL` rule. A single 0/1 term
  per predicate would make `NOT (a = 1)` hold on a `NULL` `a`, and a projected boolean FALSE where
  Postgres returns NULL.
* **No cast is erased.** Every cast in the IR is an uninterpreted function of its operand. The
  fork erases casts, which equates `CAST(a AS REAL) / b` with `a / b`; and a cast between equal IR
  types is not treated as an identity either, since the frontend drops the ones that are.
* **Functions are not assumed strict.** Only functions known to be `NULL` exactly when an argument
  is derive their nullness; any other function — parameter carriers included, since a parameter
  may be bound to `NULL` — gets an uninterpreted nullness of its own.
* **Rules that are unsound as written in the fork are left out**, and sums compare as true
  multisets (`UAdd.equals` is a one-way set comparison under which `a + a` equals `a + b`).

So the two disagree in both directions: some pairs the fork proves `sqleq-solver` does not (the
LIA\* rung, pairs that rely on a parameter never being `NULL`, and the false proofs listed at the
top), and some `sqleq-solver` proves the fork does not (rewrites the fork does not normalize, and
pairs where it runs out of time). Either way an `EQ` is a claim to be checked against `sqleq-fuzz`.

### How it is checked

`sqleq-solver/examples/phase2_gate.rs` runs the ladder over a job file and joins it row by row
against `IrDriver`'s results and a file of fuzz verdicts (one `{name, fuzz: {verdict}}` per line),
failing on any `EQ` over a pair the fuzz axis refutes. `sqleq-solver/examples/normalize_check.rs`
evaluates each side before and after normalization, and both
sides of every proved pair, on small random databases that respect the schemas' column types and
constraints, using the crate's concrete evaluator. The crate's unit tests pin the three-valued truth
tables against a reference evaluator.

### Building

`cargo build --release -p sqleq-solver` compiles Z3 from source and links it statically, so the
binary needs nothing at run time; the first build takes minutes and needs cmake and a C++20
compiler.

## The JVM fork

The original SQLSolver, run through `tools/sqlsolver/` over a fork with Calcite removed, kept as the
backup cross-check described at the top, and being deprecated. `sqleq-check --sqlsolver-jvm` finds
it through two environment variables:

| variable | what |
| --- | --- |
| `$SQLEQ_SQLSOLVER` | the fork with Calcite removed, its classes compiled into `build/classes-javac`; `--sqlsolver-tree DIR` overrides it |
| `$SQLEQ_SQLSOLVER_DEPS` | the upstream fat jar, exploded, with `org/apache/calcite` removed — the fork's classpath, and what keeps the removal honest, since a surviving reference could not resolve |

Nothing upstream is modified by any of this, and nothing is filed against it. Where the sections
below say SQLSolver, they mean this Java implementation.

### The `UPDATE` unsoundness on the SQL-text path

**Any two `UPDATE` statements against the same table are reported `EQ`, whatever they do.**

`VerificationImpl.getVerifyResult` consults `PlanSupport.isLiteralEq` *before* it does any solving,
on both of its branches. `isLiteralEq` re-parses both sides with the legacy WeTune MySQL parser and
compares the assembled plans structurally — and that grammar reduces an `UPDATE` to its bare table
reference:

```
UPDATE t SET a = 1  WHERE b = 2 AND c = 3   ->  ast = `t`   plan = Input{t AS t}
UPDATE t SET a = 99 WHERE b = 2             ->  ast = `t`   plan = Input{t AS t}
isLiteralEq = true          -> EQ, with neither the SET list nor the WHERE clause compared
```

Confirmed against the jar with plain literals and a hand-written schema, no parameters and none of
our encoding involved: differing `SET` lists, differing `WHERE` clauses, a `WHERE` against no
`WHERE`, and a correlated subquery against no predicate all return `EQ`. Two *different* tables are
compared, so that pair answers `UNKNOWN`; `RETURNING` is rejected by the legacy grammar outright.
`INSERT` and `DELETE` truncate to the same bare table, but `assemblePlan` then returns null and the
pair falls through to `UNKNOWN` — safety by accident, not by design.

`SELECT` is unaffected. The legacy parser either parses a query in full or returns nothing; it
never silently truncates one. The single clause it drops is `FOR UPDATE`, which changes locking and
not the returned rows. `LIMIT`, `OFFSET`, `DISTINCT`, `ORDER BY`, `UNION`/`UNION ALL` and `HAVING`
were each checked and all survive.

**Why it cannot reach a verdict here.** The collapse lives in `isLiteralEq`, which reads SQL text,
and nothing in this repository asks the fork about SQL text. On the bridge, the frontend reduces a
DML pair to the queries computing its effect before lowering it (`src/dml.rs`), so `IrDriver` only
ever sees two queries, and its plan entry does not call `isLiteralEq` at all. The frontend's
SQL-text jobs (`sqleq-frontend --sqlsolver` without `--ir`) still carry a `non-select-statement`
note on any row whose either side is not a `SELECT`/`WITH`/`VALUES`/`TABLE` query
(`sqlsolver::is_query`), for whoever runs them through the fork's own SQL entry point. The rule is
the structural one — this prover does not model DML — rather than a list of the keywords that happen
to be dangerous today, so `INSERT` and `DELETE` are marked too even though they currently answer
safely.

### Parameters: `$1` becomes `_DOLLAR_1()` on the SQL-text path

On the bridge a parameter arrives as the frontend's nullary carrier `qpN`, which `IrToRel` turns
into an uninterpreted function like any other unknown operator, minted under one name on both sides.
The SQL-text jobs need an encoding of their own, and this section is about them.

`SqlSupport.parsePreprocess` rewrites `$` to `_DOLLAR_`, which turns `$1` into the bare identifier
`_DOLLAR_1` and fails Calcite validation; there is no dynamic-parameter support anywhere in the
tree. The encoding that works instead is a **0-ary unresolved function**:

* `CalciteSupport.addUserDefinedFunctions` auto-registers any unknown operator as a UDF.
* An unresolved call becomes `UFunc(NON_INT, name, [])` — an uninterpreted function term.
* `LiaStarTranslator` maps it through `uTermToLiaVar.computeIfAbsent(exp, …)`, keyed on the UTerm,
  so structurally-equal terms on the two sides collapse to the **same** LIA variable.

A 0-ary uninterpreted function is a free constant, and a free constant shared by both sides is
exactly parameter semantics: the obligation becomes `∀ params. Q₁(params) ≡ Q₂(params)`. It must
never degrade to substituting a literal, which would prove equivalence at one value and claim it
for all.

**The encoding stops at `LIMIT` and `OFFSET`.** Calcite's grammar accepts only an unsigned integer
literal in those positions, so `LIMIT _DOLLAR_1()` is a parse error — a function call is not a
literal, whatever it returns. On the text path that alone keeps many parameterized queries from ever
being read, and it is **not soundly fixable by substitution**, for the same reason as above. Lifting
it needs either dynamic-parameter support in the grammar or a rewrite that moves the bound out of
the syntactic slot, neither of which is ours to make.

Binding is **by index** — `$1` on the left is `$1` on the right — the same choice the qed and fuzz
axes make. See [SOUNDNESS.md](SOUNDNESS.md).

### The schema must be MySQL-dialect DDL

`CalciteSupport` hardcodes `DB_TYPE = MySQL`, so `-schema` is parsed by their MySQL ANTLR grammar.
On the bridge the schema is rendered from the IR's own `schemas`, so the table a scan index names is
the table Calcite resolves; on the text path, from the catalog `pgddl::parse_provided_schema`
builds. Either way nothing new parses Postgres, and `sqlsolver::emit_mysql` writes backticked
identifiers, one `CREATE TABLE` per table.

The type vocabulary is closed and load-bearing. `pgddl::map_pg_type` yields five types plus the
temporal ones, mapped as:

| ours | emitted |
|---|---|
| `INTEGER` | `int` |
| `DATE`, `TIME`, `TIMESTAMP`, `TIMESTAMPTZ` | `int` |
| `REAL` | `double` |
| `VARCHAR` | `varchar(255)` |
| `BOOLEAN` | `boolean` |
| anything else | `varbinary(255)` |

Validated against the jar: `json`, `uuid`, `inet`, `money` and `xml` are all rejected by their
grammar, and **one unmappable type name kills the whole `CREATE TABLE` and therefore the whole
schema**. That is why the fallback is a type that parses rather than the Postgres spelling.

The temporal types are `int` because each is exact as an integer in its own unit, and the IR never
lets two of them meet except through a `q_conv_*` call, which `IrToRel` turns into an uninterpreted
function like any other unknown operator (see [SOUNDNESS.md](SOUNDNESS.md)). SQLSolver erases every
`CAST`, so a conversion written as one would vanish. INTERVAL, which may count months, is not linear
in one unit and falls through to `varbinary(255)`.

#### `UNIQUE` means something different here

SQLSolver models `UNIQUE` as "no duplicate rows at all". Postgres allows any number of NULLs in a
unique column. With a nullable unique `a`, SQLSolver reports `SELECT DISTINCT a FROM t` ≡ `SELECT a
FROM t` — **false in Postgres**. So the emitter declares `UNIQUE` only when every column of the key
is `NOT NULL`.

#### Schema qualifiers must come off

Calcite's root schema here is flat — `calciteSchema.add(table.name(), calciteTable)`, no
sub-schemas — so `public.t` cannot resolve, and `sqleq-fuzz`'s trick of creating the table under
its qualified name does not transfer. The emitter strips qualifiers at the token level, anchored on
"the first part that names a declared table".

Stripping is a renaming, and a renaming is only safe when it is injective: if `a.t` and `b.t` both
reduce to `t`, two distinct relations become one and a non-equivalent pair could read as
equivalent. On the text path `qualifier_conflict` detects that and leaves both sides qualified —
they then fail to resolve, so no proof can come out either. The bridge has no qualified spelling to
fall back on, since a scan names its table by index, so a plan whose tables share a bare name is
refused outright.

### The IR bridge — feeding it our plans instead of SQL text

Upstream SQLSolver re-derives a plan by parsing SQL text with Calcite/Babel in MySQL dialect, and
on our SQL that parser, not the prover, is where it mostly lost. The bridge removes the round trip:
`sqleq-frontend --sqlsolver --ir` emits `{name, ir, schema}`, where `ir` is the same `Input` JSON
the qed axis consumes; `tools/sqlsolver/IrToRel.java` turns it into a Calcite `RelNode` pair
directly, and `IrDriver` hands that pair to the `Verification.verify(RelNode, RelNode, Schema)`
overload. No SQL text is on the path, and the QED prover and the fork read the same bytes — which is
the point: re-lowering the SQL for another prover would put a second lowering between the two axes
and reintroduce exactly the drift the cross-check exists to rule out.

### Operational constraints

`sqleq-check` gets each of these right for you; they are written down because getting any one wrong
looks like a wrong answer rather than an error.

* **Classpath, not module path.** `api/module-info.java` exports `sqlsolver.api` but not
  `sqlsolver.api.entry`, so `Verification` is unreachable under JPMS. Classpath execution ignores
  it.
* **cwd must be the SQLSolver repo root.** `sqlsolver.properties` and `sqlsolver_data/` resolve
  relatively.
* **Both library paths.** `LD_LIBRARY_PATH=<repo>/lib` *and* `-Djava.library.path=<repo>/lib`.
* **Not reentrant.** Process-global state (`TEMP_TABLE_NAME_SEQUENCE`, `TEMP_COL_NAME_SEQUENCE`,
  `UMulImpl.useWeakEquals`, `USER_DEFINED_FUNCTIONS`) means parallelism must be process-level:
  `--sqleq-solver-jobs` runs separate driver processes, never threads in one.
* **Native Z3 can outrun a thread interrupt**, hence nested caps: the `sqlsolver.z3.timeout`
  property, and the driver's per-row worker join with its grace period. A worker that will not stop
  gets `Runtime.halt(3)` — so `rc == 3` is an expected exit, not a failure, and whatever drives the
  JVM has to re-read the results file and re-ask only the names still missing.
* **Per-pair schema.** `Verification.verify(sql0, sql1, schema)` re-reads the schema per call, so
  the CLI's one-schema-per-batch mode is unusable for a corpus where rows carry their own DDL.
* **No jar is run.** The fork's classes are compiled with `javac` into `build/classes-javac` and run
  against the exploded dependency directory; the bridge itself is compiled with `javac --release
  17`.
