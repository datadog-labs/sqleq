# What a verdict rests on

`sqleq` answers "are these two queries equivalent?" — and a `provable` answer is only worth the
argument behind it. This document is that argument: what the tool refuses to do rather than guess,
why refusing is the right trade, what a proof leaves out, and the single place where it assumes
something it cannot check.

Read [VALIDATION.md](VALIDATION.md) first if you want the method — how the axes check each other,
and the defects that has caught. This page is the narrower question of what any one verdict means.

## The frontend refuses rather than guess

The prover is sound: it only proves genuinely-equivalent pairs **given faithful IR**. So a false
positive can only come from unfaithful lowering. The frontend therefore **refuses** (returns an
error) any construct it cannot lower faithfully — `LIMIT`/`OFFSET` and `DISTINCT ON` (both turn on
an ordering the prover's IR has no `Sort` to express; `DISTINCT ON` keeps one row per key chosen by
`ORDER BY`, so it drops *values*, not just duplicates), window functions, `HAVING` it can't express,
correlated columns it can't resolve, `INTERSECT/EXCEPT ALL`, `LIKE ... ESCAPE`, set-returning
functions (they expand one row into many, so modelling the call as a scalar understates
cardinality), `LATERAL`, `TABLESAMPLE`/`WITH ORDINALITY`, etc. — rather than emitting best-effort
IR. It never panics or exits on bad input.

Refusing has a price, and it is paid deliberately. The set-returning-function guard, for instance,
gives up four pairs the frontend previously proved; all four are pairs whose two sides are textually
identical, which is exactly why the understated cardinality cancelled and no unsound proof resulted.
Losing a reflexive proof is the correct trade against leaving the hole open.

The same reasoning sets the direction of schema inference. A key or a `NOT NULL` *shrinks* the space
of instances the prover quantifies over, so inventing one could turn a non-equivalence into a
`provable`. Constraints are therefore only ever read off the DDL, never guessed; a missed one costs
completeness, not soundness. Two such misses are known and measured: `pgddl` does not read keys
declared by `CREATE UNIQUE INDEX`, and the catalog does not treat `SERIAL` as implying `NOT NULL`.
Between them they account for nearly all the remaining functional-dependence refusals — completeness
work, in the safe direction.

### Dates and timestamps are not one integer

A DATE counts days, a TIMESTAMP counts microseconds, and an INTERVAL may count months. The frontend
used to lower all of them as `INTEGER`, and both provers then accepted pairs that are not equivalent:
`ts < d + 1` against `ts <= d` (over integers `x < y + 1` is `x <= y`, but `d + 1` is the next *day*),
and `CAST(ts AS DATE) = d` against `ts = d` (the cast truncates, and it was dropped as an identity).
One row with a mid-day `ts` separates each pair. Nothing in the pipeline disagreed about these; they
were found by probing the lowering directly, and the disprover confirms each witness.

The rule now is that a comparison of two values of one temporal type stays native, and every
*operation* on a temporal value is an uninterpreted function. Comparisons are exact: the values of
one type, `-infinity` and `infinity` included, form a bounded total order, so they embed
order-preservingly into the integers a prover reads them as.

Arithmetic is not exact there, because of the infinities. Postgres leaves `infinity` unchanged under
`date + integer`, so `'infinity'::date + 1 = 'infinity'`, and it raises an error for
`infinity - date`. Read as integer addition, `d + 1 > d` would hold for every date, and
`d >= $1 AND d < $1 + 1` would mean `d = $1`; at `d = $1 = 'infinity'` neither does. So
`date ± integer`, `date - date` and all interval arithmetic (an interval may count months, and
months have no fixed length) are functions named after their operand types (`q_arith_add_date_integer`).
So is every crossing between two types — the promotion in `d < ts`, an explicit cast, a literal cast
such as `'2024-01-01'::date` (`q_conv_date_timestamp`) — and never a `CAST`, because both provers erase
casts they consider trivial. A function nobody interprets can only cost a proof; a prover that knows
its Postgres meaning, infinities included, can interpret the name.

For the same reason a truncation is compared as it stands. `ts::date = $1` and the range written out
by hand, `ts >= $1::date AND ts < $1::date + 1`, look like the same predicate and are not: at
`ts = $1 = 'infinity'` the truncation holds and `ts < 'infinity'` does not. A restatement of one as
the other would have made the pair lower to one term and every prover call it equivalent.

An `UPDATE` of a temporal column converts the assigned value to the column's type, as Postgres's
assignment cast does, so `SET d = ts` and `SET d = ts::date` store the same thing. That is done where
the value's type is evident from its shape (a column, a literal, a parameter, a cast); elsewhere the
value keeps its own type, which costs exactness and not soundness. Where a relation
would put two temporal types in one column with no comparison to hang a conversion on — a set
operation, a `VALUES` list, `ts IN (SELECT d …)` — the pair is refused.

### Shapes that look like something simpler

A few constructs read like a simpler one and compute something else, and each is either lowered as
what it is or refused:

- **A typmod is a computation.** `$1::varchar(2)` truncates and `$1::timestamp(0)` rounds, so only an
  *unqualified* cast over a parameter is dropped as the parameter's type. A qualified cast, over a
  parameter, a literal or anything else, is a function named after the full spelling of its target.
- **An array is not its element type.** An array column is opaque whatever it holds, and a cast to an
  array type is a function named after it, never the identity (`ys::int[]` parses); `||` over an
  opaque operand is a function, not text concatenation, because array `||` is not strict
  (`'{a}' || NULL` is `{a}`); and `x = ANY(ARRAY[..])` is expanded into comparisons only when every
  element is a scalar, since over `ARRAY[arr]` it ranges over the leaves.
- **A row against a parameter is a record comparison.** In `(a, b) IN ($1, ..)` each parameter
  stands for a composite value, and Postgres compares a row with one under record semantics, where
  two NULL fields are equal. Each such item is one opaque predicate, never per-field comparisons.
- **A subscript chain is not a composition.** `m[1][2]` is one function of `m` and both indices:
  over a two-dimensional array `(m[1])[2]` is NULL, because `m[1]` has too few subscripts. Slices
  are refused, and so is an `INTERVAL` with a field qualifier, which changes how its string is read.
- **A quantified pattern is not a pattern.** `s LIKE ALL($1)` is refused: `NULL LIKE ALL('{}')` is
  TRUE, so it is not a strict `LIKE` against one opaque pattern.
- **A set-returning function is not a scalar** in any position, over aggregates included.
- **A join-delete or join-update is a semi-join only when nothing it assigns or returns reads the
  join.** `DELETE FROM t USING u WHERE p` deletes the rows `EXISTS (SELECT 1 FROM u WHERE p)` keeps,
  but when several `u` rows match, a `SET` or `RETURNING` reading `u` takes an unspecified one of
  them. Those are refused, and so is a bare `RETURNING *`, which reaches `u`'s columns.
- **A quoted name keeps its case.** Names resolve case-insensitively, which is Postgres's rule for
  unquoted names only, so a schema with two tables, or two columns of one table, whose names differ
  only in case (`"s"` and `"S"`) is refused rather than resolved to one of them.
- **The target of a `DELETE` or `UPDATE` always names the table.** A `WITH` binding of the same name
  would be inlined over it by the reduction, so `WITH t AS (…) DELETE FROM t`, which empties `t`, is
  refused rather than lowered as a filtered delete.
- **`USING` merges columns.** `SELECT *` over `JOIN … USING (k)` has one `k` where the `ON` form has
  two, so it is refused. After a `RIGHT` or `FULL` join has merged `k`, the merged column is a
  coalesce of both sides, so a further `USING (k)` is refused rather than compared with one of them.
- **An alias's column list renames by position.** In `t AS x(b, a)`, `x.b` is `t`'s first column,
  whatever that column is called. A list that leaves two columns with one name is refused.
- **Parentheses in a `FROM` clause group.** `a LEFT JOIN (b JOIN c ON p) ON q` is lowered with its
  grouping, since it is not `(a LEFT JOIN b ON q) JOIN c ON p`, and the inner `ON` sees only the
  inner join's own tables. An aliased one, `(b JOIN c) AS x`, is refused.

## A query that raises an error

Neither prover models runtime errors: both assume every operation yields a value. So a proof says
that the two queries return the same rows on every database on which both run without an error,
and nothing about which databases make one of them fail. A stronger claim would not be well
defined for Postgres, which does not fix the order in which it evaluates a query's conditions:
whether `b <> 0 AND a / b > 1` raises a division by zero depends on the plan, not on the data.

Dates show this at the top of their range. A DATE reaches the year 5874897 and a TIMESTAMP only
294276. Compared with a timestamp, a later date orders above every finite one and below
`infinity`; cast to a timestamp, it raises an error. The frontend lowers both through one
conversion, so `d < ts` and `d::timestamp < ts` lower alike, and they do return the same rows
wherever the cast succeeds.

## The one assumption: `$N` on one side is `$N` on the other

Everything above is the frontend declining to lower what it cannot lower faithfully. This section is
the one place it **assumes** instead, because the fact it needs is not in the input at all.

A parameterized pair arrives as two SQL strings with `$1, $2, …` in them, and a schema. The frontend
lowers `$N` to a single shared symbol `qpN` — one value per execution, the same value everywhere it
appears and the same on both sides — so the question the prover is handed is **index binding**: *for
every value of `qp1, qp2, …`, do the two queries agree?*

What the caller means is **intended binding**: `$1` on the left is whichever placeholder on the
right the application fills from the same value. The two coincide exactly when both sides were
numbered from the same call site. A rewrite that drops, adds, or reorders a placeholder renumbers
every placeholder after it — and then `$2` on the left and `$2` on the right are two *different*
application values that the frontend has collapsed into one symbol. Collapsing them makes the prover
check only the diagonal of the space the real question ranges over, and report that as a general
proof. This is the one soundness hole in the frontend that is not a lowering bug, and it cannot be
closed here: a parameter mapping exists only in the application, and the frontend never sees the
call site.

So the frontend assumes the identity mapping and reports the misalignments it can detect, as
`parameter-misaligned` with a sub-reason (`src/params.rs`):

* **`arity`** — the two sides mention different sets of `$N`, *and* share at least one. Exact, no
  heuristics. The overlap condition is not a weakening: when no index occurs on both sides, index
  binding quantifies the two queries over *independent* values, which is a **stronger** statement
  than any correspondence the caller could have meant, so it can only fail to prove something true.
  That is what `LIMIT $1 OFFSET $2` against a side with no parameters is — the shape the crate's
  `a_parameter_is_a_count` test pins, and the broad rule refuses it wrongly. Measured, the broad
  rule costs very few rows, all of them already undecided, because most pagination rewrites keep a
  filter parameter as well and so overlap anyway; the case for the condition is the argument, not
  the row count. Overlap is what turns a difference into a hazard, because a shared index is the one
  thing a renumbering would have moved.
* **`order`** — the same set on both sides, but some `$k` is compared against a disjoint set of base
  columns on the two sides. Read off inference's own attribution, not off the SQL text. **Best
  effort.**

`arity` is syntactic, so it has a verdict even on a pair type inference cannot get through — and
there the two refusals are not independent. Identifying `$k` across two queries that number their
placeholders differently puts two unrelated application values in one type class, and the `type
conflict` that falls out is one this frontend's own assumption manufactured. So: **a misalignment
outranks any refusal it could have manufactured, and yields to any refusal it could not.**

Which is which is decided by a counterfactual, not a guess: the second query's parameters are
renumbered clear of the first's (`infer::split_params`) and the pair is put through the same stages
again. Renumbering drops exactly one thing — the identification of the two queries' `$N` — and keeps
everything else, including the single union-find spanning the pair for *columns*, so a disagreement
inside one query or between the two queries' column evidence survives it and is still reported as
the conflict it is.

The rule applies at both gates the held verdict creates, because both can be manufactured:

* **inference** (`params::root_cause`) — a `type conflict` at a parameter that is not itself wrong.
* **lowering** (`params::root_cause_lowered`) — a refusal whose message names an inferred *type*.
  The clearest is the `LIMIT`/`OFFSET` count guard: a pair comparing `is_deleted = $4 LIMIT $5`
  against `is_deleted = false LIMIT $4` puts a boolean filter value and a row count in one type
  class under index binding, and the count comes out `BOOLEAN`. Refusals about a *construct* are
  almost every other row that reaches this gate with a verdict pending, and they all yield: no
  renumbering invents or removes a window function. The counterfactual is a real re-run rather than
  a list of type-dependent guards, so it cannot go stale when the next one is added.

Nothing about a verdict turns on any of this — the pair is refused either way — only on which fact
the caller is handed first, and therefore which bucket the row is counted in.

**The detection is not complete. A misalignment it misses can yield a `provable` verdict for a pair
that is not equivalent under the binding the caller intended.** `order` has evidence only where a
parameter meets a column under a comparison, and it fires only when both sides' evidence is
non-empty and disjoint. A permutation between two columns of the *same inferred type*, in positions
the walk cannot tell apart, leaves no trace: the role sets coincide or are empty, the pair lowers,
and it may prove. Inference catches the differently-typed version of this incidentally — one
union-find is shared across the pair, so a parameter unified with an `INTEGER` column on one side
and a `VARCHAR` column on the other is a `type conflict` refusal — which is exactly why what remains
is the same-type case. That still holds with the precedence rule above: a permutation of *equal*
sets never fires `arity`, so nothing is there to outrank the conflict, and the pair is refused by
inference exactly as it was.

Two consequences worth stating plainly:

* A `provable` verdict on a parameterized pair is a claim about **index binding**. It is a claim
  about the caller's pair only insofar as the caller's mapping is the identity.
* This is not hypothetical. The worked example is a corpus pair with seven placeholders against two,
  sharing `$1` and `$2`, with `$1` a `count()` argument on one side and `accounts.id` on the other. The
  prover reported `provable`, and a two-row table refutes the pair under index binding. It now
  reports `parameter-misaligned: arity` and is never lowered.

### What the check cost, and what it did not

Adding the check moved rows out of the emitted set and into a refusal bucket. Three properties of
that measurement are worth keeping here, because they are arguments rather than counts:

* **Most misaligned rows are rows nothing else would have refused.** That is what raising the
  verdict *after* lowering buys: a construct the frontend cannot lower still outranks a
  misalignment, so the bucket measures the exposure of this gap rather than reshuffling rows already
  refused.
* **No row's status changed in either direction.** The refused set came out the same set,
  relabelled, and the emitted cases byte-identical — which is why the change needed no `sqleq-fuzz`
  cross-check: nothing new reached the prover.
* **`order` reporting nothing was checked, not assumed.** Thousands of pairs reach it with a shared
  parameter carrying role evidence on both sides, and most shared parameter slots are doubly
  evidenced. So the corpus contains no *detectable* permutation. That says nothing about the
  undetectable ones, which is the hole above.

There is also an ordering hazard worth naming, because it bit once: the `$N` sets must be
snapshotted **before** the equivalence-preserving normalizations, not read off the trees they leave
behind (`params::mentioned`). `strip_identical_pagination` deletes a `LIMIT` the two sides share and
is pair-level, so on one pair it took query A's only `$4` and left the two inside query B's `UNION
ALL` — firing `arity` on a pair whose two texts both mention `$1..$4`, a misalignment this crate had
manufactured itself.

### Why it is a refusal and not a refutation

The cost is paid in the same direction as every other refusal here. `arity` is exact, so its cost is
pairs whose extra `$3` occurs solely as `SELECT $3` inside an `EXISTS` where nothing observes it —
genuinely equivalent, refused because *equivalent* is not something the check is in a position to
know. One refinement that would keep those benign rows is tempting and the corpus refutes it: "the
orphan indices all sit above every shared index, so dropping them cannot have renumbered the shared
ones" also readmits the false proof above, whose orphans `$3..$7` all sit above the shared `$1` and
`$2`. It is exactly wrong on the one case that matters. `order` can likewise fire on a legitimate
rewrite that moves a comparison onto a join partner (`a.id = $1 AND a.id = b.a_id` against `b.a_id =
$1` has genuinely disjoint role sets). Both lose proofs; neither invents one.

`parameter-misaligned` is deliberately **not** a claim of non-equivalence, even though a renumbered
pair usually is non-equivalent under index binding. The benign rows above are the counterexample to
that shortcut. Misalignment is evidence about the *question*, not about the answer, so it gets its
own reason and its own bucket in every harness that reports refusals. It is also reported **last**, after both
queries have lowered, so the bucket counts rows nothing else would have refused.

## Reading further

* [VALIDATION.md](VALIDATION.md) — how the tool is validated, and the defects that has caught.
* [DESIGN.md](DESIGN.md) — why the frontend is built this way.
