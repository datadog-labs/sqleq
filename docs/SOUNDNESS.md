# What a verdict rests on

`sqleq` answers "are these two queries equivalent?" — and a `provable` answer is only worth the
argument behind it. This document is that argument: what the tool refuses to do rather than guess,
why refusing is the right trade, what a proof leaves out, and the two places where it assumes
something it cannot check.

Read [VALIDATION.md](VALIDATION.md) first if you want the method — how the axes check each other,
and the defects it has caught. This page is the narrower question of what any one verdict means.

## The frontend refuses rather than guess

The provers are taken to be sound: each proves only genuinely equivalent pairs **given faithful IR**
(the JVM SQLSolver has known exceptions, listed in [SQLSOLVER.md](SQLSOLVER.md)). So, short of the
two assumptions below, a false positive can only come from unfaithful lowering. The frontend
therefore **refuses** (returns an error) any construct it cannot lower faithfully — window
functions, `HAVING` it can't express, correlated columns it can't resolve, `INTERSECT/EXCEPT ALL`,
`LIKE ... ESCAPE`, set-returning functions (they expand one row into many, so modelling the call as
a scalar understates cardinality), `LATERAL`, `TABLESAMPLE`/`WITH ORDINALITY`, `FETCH ... WITH
TIES`, a `WITH` it cannot inline (`RECURSIVE`, or one with a data-modifying binding, at any level),
etc. — rather than emitting best-effort IR. It never panics or exits on bad input: a statement whose
expressions or set operations nest more than 1,024 levels deep, which the parser builds with a loop
from a long chain such as `a + a + … + a`, is refused before any pass recurses on it.

Two constructs that turn on an ordering are lowered rather than refused. A row slice — `LIMIT`,
`OFFSET`, `FETCH FIRST` — becomes the prover's `Sort` node, carrying the whole `ORDER BY` in clause
order (`src/lower.rs`, `apply_pagination`). `DISTINCT ON` becomes a group whose every output column
is an uninterpreted aggregate over the candidate rows and the values that order them
(`lower::distinct_on`); it keeps one row per key *chosen by* `ORDER BY`, so it drops values, not
just duplicates, and an uninterpreted aggregate admits more behaviours than the real operator, never
fewer. `DISTINCT ON` over an aggregate query or under a set operation is still refused. Both rest on
one assumption the IR cannot avoid, and it is the one way either lowering can yield a proof the
database does not license; see [below](#a-row-slice-is-taken-as-deterministic).

Refusing has a price, and it is paid deliberately. The set-returning-function guard, for instance,
gives up proofs the frontend once made, on pairs whose two sides are textually identical — which is
exactly why the understated cardinality cancelled and no unsound proof resulted. Such a pair is not
lost outright: when a refused pair's two sides normalize to one query, the frontend says so
(`sqleq_frontend::reflexive`), without lowering anything — a query is equivalent to itself, however
many `now()`s it contains. `sqleq-check` records that claim as `reflexive`; the pins and
`--portfolio` (as `equivalent`) credit it, while the default `--expect equivalent` policy, which
asks whether the QED prover proved the pair, still counts it as refused. The claim needs every
normalization to keep the number of times a call is evaluated, and `WITH` inlining does not: it
evaluates a binding read twice twice, where Postgres evaluates it once. So the check declines a pair
in which inlining would copy a volatile call such as `random()`.

The same reasoning sets the direction of schema inference. A key or a `NOT NULL` *shrinks* the space
of instances the prover quantifies over, so inventing one could turn a non-equivalence into a
`provable`. Constraints are therefore only ever read off the DDL, never guessed; a missed one costs
completeness, not soundness. Two such misses are known: `pgddl` does not read keys declared by
`CREATE UNIQUE INDEX`, and the catalog does not treat `SERIAL` as implying `NOT NULL`. Both cost
functional-dependence refusals — completeness work, in the safe direction. And a key reaches a prover
only when every one of its columns is `NOT NULL`: a prover reads a key as "two rows agreeing on these
columns are one row", and Postgres admits any number of rows whose `UNIQUE` column is NULL, so
`SELECT u` and `SELECT DISTINCT u` over a nullable unique `u` are not one query. Nor is a key read
from a `DEFERRABLE` constraint (`INITIALLY DEFERRED`, which implies it, included): Postgres checks
it when the transaction commits, or once a transaction defers it, so a query inside the
transaction can see two rows that agree on it. The `NOT NULL` a deferrable `PRIMARY KEY` implies is
checked at once, and is kept.

### Dates and timestamps are not one integer

A DATE counts days, a TIMESTAMP counts microseconds, and an INTERVAL may count months. The frontend
used to lower all of them as `INTEGER`, and the provers then accepted pairs that are not equivalent:
`ts < d + 1` against `ts <= d` (over integers `x < y + 1` is `x <= y`, but `d + 1` is the next
*day*), and `CAST(ts AS DATE) = d` against `ts = d` (the cast truncates, and it was dropped as an
identity). One row with a mid-day `ts` separates each pair. Nothing in the pipeline disagreed about
these; they were found by probing the lowering directly, and the disprover confirms each witness.

The rule now is that a comparison of two values of one temporal type stays native, and every
*operation* on a temporal value is an uninterpreted function. Comparisons are exact: the values of
one type, `-infinity` and `infinity` included, form a bounded total order, so they embed
order-preservingly into the integers a prover reads them as.

Arithmetic is not exact there, because of the infinities. Postgres leaves `infinity` unchanged under
`date + integer`, so `'infinity'::date + 1 = 'infinity'`, and it raises an error for `infinity -
date`. Read as integer addition, `d + 1 > d` would hold for every date, and `d >= $1 AND d < $1 + 1`
would mean `d = $1`; at `d = $1 = 'infinity'` neither does. So `date ± integer`, `date - date` and
all interval arithmetic (an interval may count months, and months have no fixed length) are
functions named after their operand types (`q_arith_add_date_integer`). So is every crossing between
two types — the promotion in `d < ts`, an explicit cast, a literal cast such as `'2024-01-01'::date`
(`q_conv_date_timestamp`) — and never a `CAST`, because a prover may erase a cast it considers
trivial (the JVM SQLSolver erases every one). A function nobody interprets can only cost a proof; a
prover that knows its Postgres meaning, infinities included, can interpret the name.

For the same reason a truncation is compared as it stands. `ts::date = $1` and the range written out
by hand, `ts >= $1::date AND ts < $1::date + 1`, look like the same predicate and are not: at
`ts = $1 = 'infinity'` the truncation holds and `ts < 'infinity'` does not. A restatement of one as
the other would have made the pair lower to one term and every prover call it equivalent.

An `UPDATE` of a temporal column converts the assigned value to the column's type, as Postgres's
assignment cast does, so `SET d = ts` and `SET d = ts::date` store the same thing. That is done
where the value's type is evident from its shape (a column, a literal, a parameter, a cast);
elsewhere the value keeps its own type, which costs exactness and not soundness. Where a relation
would put two temporal types in one column with no comparison to hang a conversion on — a set
operation, a `VALUES` list, `ts IN (SELECT d …)` — the pair is refused.

### Types the provers would read with the wrong arithmetic or the wrong equality

A Postgres type is mapped onto an IR type only where the IR type's operations are the Postgres
type's. Both provers read REAL as exact rational arithmetic and any type's `=` as equality, so:

- **`numeric` is REAL, but its division is not exact, and REAL has no scale.** Addition,
  subtraction and multiplication of numerics are exact; division rounds to a finite scale
  (`1 / 3.0 * 3.0` is `0.99…990`), so `/` over a REAL is the uninterpreted `q_arith_div_real_real`.
  A numeric's text shows its scale, `1.0` and `1.00` being one number and two strings, so a numeric
  cast to text, or concatenated with `||`, is refused unless both queries lower to one plan.
- **Floats are opaque.** `real`, `double precision` and `float` round, and float addition is not
  associative: `(0.1 + 0.2) + 0.3` is `0.6000000000000001` and `0.1 + (0.2 + 0.3)` is `0.6`. A
  float is VARBINARY, and arithmetic over any opaque operand, a float or a range or a point, is an
  uninterpreted `q_arith_<op>_<left>_<right>`. Proofs that need float arithmetic, or a float's
  order against a constant, are given up with it.
- **`citext` and `char(n)` are refused.** Their `=` ignores case or trailing spaces. No IR type has
  that equality: as VARCHAR, `'A'` and `'a'` would be different values, and as an opaque type
  their `=` would be the prover's equality, which substitutes equals for equals, so from
  `t.c = u.c` it would conclude `t.c::text = u.c::text`, which citext does not satisfy. A query
  that reads a value of either type is refused, unless the two queries lower to one plan, which
  computes the same thing however `=` is read; a column of one that no query reads costs nothing.
  `SELECT *`, `DELETE` and `UPDATE` read every column of the table they touch.
- **Integer types are matched by name.** `int4range` and `point` contain `INT` and are opaque.
  The two readers of type names, one for a declared `CREATE TABLE` and one for raw DDL and
  inference, read every name from one table, so `uuid` and `money` are opaque in both.
- **An untyped literal takes the type of what it meets.** Postgres reads `'01'` in `a = '01'`
  over an INTEGER `a` as the integer 1, and `'yes'` against a BOOLEAN as `true`. The frontend does
  the same, in comparisons, in `CASE` branches and in arithmetic, rather than comparing `a::text`
  with `'01'` as strings. Text it cannot read the way Postgres does stays an uninterpreted cast of
  the literal.

### Strings are ordered by a collation no input states

Postgres orders two strings by the collation of their comparison: the one an operand names with
`COLLATE`, else the one a column declares, else the database's default. Collations disagree: under
`en_US.utf8`, `'a' < 'b' < 'B'`; under `C`, `'B' < 'a'`. No input states the database's default,
so the provers are not told one, and assume none. Their own string order is by code point, which is
the order of `C` and `POSIX` and of no other collation Postgres guarantees.

- **An order comparison of two strings is native only under `C` or `POSIX`**, named by a column's
  `COLLATE` or by an operand's (`s < 'a' COLLATE "C"`). Under the database's default collation
  `a < b` is the uninterpreted predicate `q_str_lt(a, b)` and `a <= b` is `q_str_le(a, b)`, with
  `a > b` read as `b < a`; under another named collation, the same with the collation's name as a
  third operand, since two collations order one pair of strings differently. `BETWEEN` is its two
  comparisons. The same comparison on both sides still meets; what is lost is ordering two
  constants or chaining two comparisons, which is what proving `s > 'a' AND s < 'B'` equal to
  `FALSE` takes. As in Postgres, a column's collation outranks the default of a constant or of a
  column that declares none, so `c < 'x'` over a `COLLATE "C"` column is native. Two columns of two
  collations in one comparison, an error in Postgres, are refused, and so is a `COLLATE` naming any
  other collation, or one that is not an operand of a comparison, where it changes what `upper` or
  `ILIKE` above it compute.
- **`=` is identity only under a deterministic collation.** Every predefined collation is
  deterministic, and so is every database's default; one made with `CREATE COLLATION … (deterministic
  = false)` is not, and under it `'a' = 'A'` can hold. A column under such a collation is refused
  wherever a query reads it, as `citext` is, and so is a column under a collation the DDL does not
  create and that is not named the way Postgres names its predefined ones: `default`, `C`, `POSIX`,
  `ucs_basic`, `unicode`, `pg_c_utf8`, a libc locale such as `en_US.utf8`, or an ICU one such as
  `en-US-x-icu`. That reading is taken from the name, not checked against a database: a user can
  create a non-deterministic collation named `en_US.utf8` in a schema of their own, and Postgres
  reaches it where it predefines none of that name. A DDL that creates a collation under a
  predefined name, or creates one name twice, makes that name refused. A column of a type the IR
  keeps opaque, an array of text say, under a collation other than the default is refused as well:
  its order would need a predicate of its own.
- **Other operations read a collation too.** `min`, `max`, `greatest`, `least` and a row slice under
  `ORDER BY` read its order; `upper`, `lower`, `ILIKE` and the regular expressions its character
  classes. They are lowered with no collation in their names, which is sound while every string in
  the pair has one collation, as it does when no column the pair reads declares one. Where one does,
  an operation over strings is refused unless it is one of the comparisons above or never reads a
  collation (`=`, `||`, `LIKE`, a cast, `COALESCE`, `NULLIF`, `COUNT`, the null tests, the JSON
  lookups); so is a row slice ordered by a string, and an `ORDER BY … LIMIT` the two sides share is
  not stripped: over `SELECT c AS x` and `SELECT d AS x`, with `c = d` on every row, `ORDER BY x
  LIMIT 1` takes two different rows when `c` is `COLLATE "C"` and `d` is not. A comparison whose
  operand is neither a column nor a constant (`(c || 'x') < 'y'`) takes its collation from the
  columns it reads, which the frontend does not trace, so in such a pair it is refused too.

`sqleq-fuzz` compares strings by code point, as DuckDB does, so it cannot refute a pair whose two
sides differ only under another collation; the `witness:` of such a pinned pair names the collation
it needs.

### Shapes that look like something simpler

A few constructs read like a simpler one and compute something else, and each is either lowered as
what it is or refused:

- **A typmod is a computation.** `$1::varchar(2)` truncates and `$1::timestamp(0)` rounds, so only
  an *unqualified* cast over a parameter is dropped as the parameter's type. A qualified cast, over
  a parameter, a literal or anything else, is a function named after the full spelling of its
  target.
- **A failure-tolerant cast is not a cast.** `TRY_CAST(x AS t)` and `SAFE_CAST(x AS t)` yield NULL
  where `CAST` raises an error, and Postgres has neither. sqlparser accepts both and builds the node
  it builds for a `CAST`, so the frontend refuses them before anything else reads the tree.
- **An array is not its element type.** An array column is opaque whatever it holds, and a cast to
  an array type is a function named after it, never the identity (`ys::int[]` parses); `||` over an
  opaque operand is a function, not text concatenation, because array `||` is not strict (`'{a}' ||
  NULL` is `{a}`); and `x = ANY(ARRAY[..])` is expanded into comparisons only when every element is
  a scalar, since over `ARRAY[arr]` it ranges over the leaves.
- **A key is not a path.** `j -> 'k'` looks up one key and `j #> '{k}'` follows a path, so they
  are two uninterpreted functions, and `jsonb_extract_path(j, 'k')`, which takes the path one
  element per argument, is a third: over `[5]`, `jsonb_extract_path(j, '0')` is `5` and `j -> '0'`
  is NULL.
- **An `IN` subquery compares values of one type.** Its operand is converted to the type of the
  subquery's column (`i IN (SELECT a / 2.0 ..)` compares `i::numeric`), and where only the column
  could be converted (`n IN (SELECT i ..)` over a numeric `n`), the pair is refused, since the QED
  prover asserts that the two have one sort.
- **A row against a parameter is a record comparison.** In `(a, b) IN ($1, ..)` each parameter
  stands for a composite value, and Postgres compares a row with one under record semantics, where
  two NULL fields are equal. Each such item is one opaque predicate, never per-field comparisons.
- **A subscript chain is not a composition.** `m[1][2]` is one function of `m` and both indices:
  over a two-dimensional array `(m[1])[2]` is NULL, because `m[1]` has too few subscripts. Slices
  are refused, and so is an `INTERVAL` with a field qualifier, which changes how its string is read.
- **A quantified pattern is not a pattern.** `s LIKE ALL($1)` is refused: `NULL LIKE ALL('{}')` is
  TRUE, so it is not a strict `LIKE` against one opaque pattern.
- **A set-returning function is not a scalar** in any position, over aggregates included.
- **A constant says its value and nothing more.** A prover reads a constant's value off its name,
  so a string spelled `null` in any case, which a prover would read as SQL NULL, is emitted as a
  concatenation (`'n' || 'ull'`). A numeric literal has Postgres's type — `1e-5` and an integer past
  the `bigint` range are `numeric`, not integers — and Postgres's spelling (`.5` is `0.5`). The QED
  prover reads a decimal constant through an `f32`, so one that is not exactly such an `f32`
  (`0.1`, `20000000.5`) is an uninterpreted function of its text, and so is a string constant cast
  to a decimal type that the cast would round the same way. A number run into a name, `0b101` or
  `1x`, is refused: Postgres reads it as one token, an integer in base 2, 8 or 16 or a syntax error,
  where the parser reads a number and an alias.
- **Integer division truncates.** Postgres rounds `-7 / 2` toward zero and gives `-7 % 2` the sign
  of the dividend; a prover's integer division is Euclidean. So `/` and `%` on integers are
  functions named after their operand types (`q_arith_div_integer_integer`), never the native
  operators.
- **A volatile function is not a function.** `random()`, `nextval`, `clock_timestamp()` and every
  other function Postgres declares volatile, in its core or in `pgcrypto` and `uuid-ossp`
  (`sqleq_frontend::VOLATILE_FUNCTIONS`), can give two calls with equal arguments two values, so a
  call to one is refused rather than read as an uninterpreted function. A volatile function a user
  defines is a name like any other.
- **An aggregate is not a per-row function.** Every built-in Postgres aggregate is modelled or
  refused: modelled as the prover's own (`count`, `sum`, `avg`, `min`, `max`) or as an uninterpreted
  function of the bag of its inputs (`bool_or`, `bit_or`, `var_pop`, `corr`, `regr_*`, `range_agg`,
  …); refused when the bag does not determine its result (`array_agg`, `string_agg`, the `json*_agg`
  family, `any_value`) or when it is an ordered-set or hypothetical-set aggregate
  (`percentile_cont`, `mode`, `rank(…) WITHIN GROUP`). An aggregate in a subquery whose arguments
  read only an enclosing query's columns belongs to that query, and is refused. An aggregate a user
  defines is recognised only through a `declare aggregate function` line.
- **A call is more than a name and positional arguments.** A named argument
  (`make_interval(days => a)`, `json_object('k' VALUE a)`), a `t.*` argument, `WITHIN GROUP`, the
  SQL/JSON `ON NULL` and `RETURNING` clauses, an `ORDER BY` or `WHERE` inside the parentheses and
  `IGNORE NULLS` are refused, and so are `DISTINCT`, `FILTER` and `*` on a call that is not a known
  aggregate. `SELECT … INTO`, which creates a table, is refused too.
- **A join-delete or join-update is a semi-join only when nothing it assigns or returns reads the
  join.** `DELETE FROM t USING u WHERE p` deletes the rows `EXISTS (SELECT 1 FROM u WHERE p)` keeps,
  but when several `u` rows match, a `SET` or `RETURNING` reading `u` takes an unspecified one of
  them. Those are refused, and so is a bare `RETURNING *`, which reaches `u`'s columns.
- **A quoted name keeps its case.** A name is folded as Postgres folds it: an unquoted one to lower
  case (ASCII only, as under a multibyte server encoding), a quoted one not at all. So a column
  declared `"A"` is not read by `A`, a table alias `"X"` is not `x`, a `WITH "T"` binding is not a
  use of `t`, and an `ORDER BY A` key is not the output column `"A"`. Table names are still
  compared case-insensitively, as is the attribution of a column in type inference, so a schema with
  two tables, or two columns of one table, whose names differ only in case (`"s"` and `"S"`) is
  refused rather than resolved to one of them. So is a derived table with two columns of one name
  up to case, whether the select list, the alias's column list or a `*` named them.
- **A name is resolved where Postgres resolves it.** A qualified `s.x` reads the nearest relation
  called `s`, and when that relation has no column `x` it is refused, as Postgres raises an error,
  rather than read from an enclosing relation also called `s`. A bare name reads the query's own
  `FROM` first and an enclosing query's only when no relation of its own has the name. Postgres
  names every unaliased select-list item, and where the frontend cannot tell the name — a `CASE`,
  a cast of an expression, a scalar subquery, a column of `VALUES` — the column is one no name
  reaches. A name that misses a relation holding such a column is refused rather than looked for
  further out, since Postgres may have given the column exactly that name; a `GROUP BY` name is
  refused then too, rather than read as a select-list alias.
- **A schema qualifier is part of a table's name.** Qualifiers are dropped so that `s.t` meets the
  DDL's `t`, but only when every reference in the pair to one bare name has the same qualifier.
  `s1.t` against `s2.t`, or `t` against `s.t`, keeps its qualifiers, so the two sides read two
  tables, and a DDL that declares only `t` declares neither of them.
- **`DEFAULT` is a keyword.** In `UPDATE … SET a = DEFAULT`, or a `VALUES` row of an `INSERT`, it
  stands for the column's default, which the reductions do not model, so it is refused rather than
  read as a column named `default`.
- **The target of a `DELETE` or `UPDATE` always names the table.** A `WITH` binding of the same name
  would be inlined over it by the reduction, so `WITH t AS (…) DELETE FROM t`, which empties `t`, is
  refused rather than lowered as a filtered delete.
- **An `INSERT` is compared by the bag it adds, so what it stores must be a function of the rows it
  lists.** A pair of `INSERT`s into one table under one column list reduces to their two sources
  (`dml::insert_pair`): bag addition is cancellative, so the final tables agree exactly when the
  added bags do. That holds only if every column the list omits gets a value fixed by the row — no
  default, a literal, or a clock function, which takes one value per statement and is read, as
  everywhere in the pipeline, as one value shared by both sides. A `nextval()` default, `SERIAL`
  included, numbers rows by *position*: the same two rows inserted in two orders have equal source
  bags and leave different tables. So an `INSERT` omitting such a column is refused, and so are `ON
  CONFLICT`, `DEFAULT VALUES`, an `INSERT` with no column list, two lists that differ in content or
  order, and a `RETURNING` that is not the same list on both sides.
- **`USING` merges columns.** `SELECT *` over `JOIN … USING (k)` has one `k` where the `ON` form has
  two, so it is refused. After a `RIGHT` or `FULL` join has merged `k`, the merged column is a
  coalesce of both sides, so a further `USING (k)` is refused rather than compared with one of them.
  A `USING (k)` whose left or right side has two `k` columns that no earlier `USING` merged is
  refused, as Postgres rejects it, rather than compared with the first.
- **An alias's column list renames by position.** In `t AS x(b, a)`, `x.b` is `t`'s first column,
  whatever that column is called. A list that leaves two columns with one name is refused.
- **Parentheses in a `FROM` clause group.** `a LEFT JOIN (b JOIN c ON p) ON q` is lowered with its
  grouping, since it is not `(a LEFT JOIN b ON q) JOIN c ON p`, and the inner `ON` sees only the
  inner join's own tables. An aliased one, `(b JOIN c) AS x`, is refused.
- **A comma groups too, loosest of all.** `FROM a, b RIGHT JOIN c ON p` is `a` crossed with
  `b RIGHT JOIN c ON p`, not `(a CROSS JOIN b) RIGHT JOIN c ON p`, which keeps `c`'s rows when `a`
  is empty. Each comma item is lowered as its own join tree, and its `ON` and `USING` see only that
  tree's tables.
- **An `ORDER BY` or `DISTINCT ON` key is read as Postgres reads it.** An integer is a position in
  the select list, a bare name is the output column of that name, and anything else, a qualified
  name like `t.a` included, is an expression over the `FROM` clause. Output columns are named as
  Postgres names them: a quoted alias keeps its case, and an unaliased column, call, or cast of one
  is named after the column or function. A bare key that two output columns carry is refused, and
  so is one that matches no output name while an output column's name is one the frontend cannot
  tell. Above a `GROUP BY` or `DISTINCT` an input expression cannot be addressed, so such a key is
  refused unless the select list writes the same expression.

## A query that raises an error

The provers that read the IR do not model runtime errors: each assumes every operation yields a
value. So a proof from one of them says that the two queries return the same rows on every database
on which both run without an error, and nothing about which databases make one of them fail. A
stronger claim would not be well defined for Postgres, which does not fix the order in which it
evaluates a query's conditions: whether `b <> 0 AND a / b > 1` raises a division by zero depends on
the plan, not on the data. (The Lean axis states a claim of its own, about whole runs of the two
statements; see [LEAN.md](LEAN.md).)

Dates show this at the top of their range. A DATE reaches the year 5874897 and a TIMESTAMP only
294276. Compared with a timestamp, a later date orders above every finite one and below
`infinity`; cast to a timestamp, it raises an error. The frontend lowers both through one
conversion, so `d < ts` and `d::timestamp < ts` lower alike, and they do return the same rows
wherever the cast succeeds.

## A row slice is taken as deterministic

`LIMIT 10` with no `ORDER BY`, or with one that leaves ties, does not say which rows it returns:
Postgres may return any of the candidates, and two runs may differ. The prover's `Sort` node cannot
say that. It is a function of its source — equal sources give equal slices — so two sides that take
the same slice of rows the prover can show equal are proved equal, as if the database made the same
choice both times. `DISTINCT ON` with no `ORDER BY`, or with one that leaves ties within a key,
keeps an arbitrary row per key and is read the same way (`src/lower.rs`, `apply_pagination` and
`distinct_on`).

That is the prover's abstraction and the standard one, and the frontend inherits it rather than
widening it; `normalize::strip_identical_pagination`, which removes a top-level `ORDER BY … LIMIT …`
identical on both sides, rests on the same reading, and is skipped where a column declares a
collation, under which equal outputs need not be ordered alike
([above](#strings-are-ordered-by-a-collation-no-input-states)). What it licenses is narrow. A proof over a slice
that ties leave open says the two sides agree whenever the database settles the ties the same way
for both — not that either returns the rows you meant. A difference in the pagination itself —
another count, offset or ordering — lowers to a different term, and goes unproved unless the two are
equal for a reason the prover can see (`LIMIT 0` is empty, `OFFSET 0` is no offset).

## The parameter assumption: `$N` on one side is `$N` on the other

Everything else in this document is the frontend declining to lower what it cannot lower faithfully,
or, in the section above, reading a slice the way the prover does. This section is the other place
it **assumes**, and the one where the fact it needs is not in the input at all.

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
proof. This is a soundness hole of the frontend's own — neither a lowering bug nor an abstraction it
shares with the prover — and it cannot be closed here: a parameter mapping exists only in the
application, and the frontend never sees the call site.

So the frontend assumes the identity mapping and reports the misalignments it can detect, as
`parameter-misaligned` with a sub-reason (`src/params.rs`):

* **`arity`** — the two sides mention different sets of `$N`, *and* share at least one. Exact, no
  heuristics. The overlap condition is not a weakening: when no index occurs on both sides, index
  binding quantifies the two queries over *independent* values, which is a **stronger** statement
  than any correspondence the caller could have meant, so it can only fail to prove something true.
  That is what `LIMIT $1 OFFSET $2` against a side with no parameters is — the shape the crate's
  `a_parameter_is_a_count` test pins, and the broad rule would refuse it wrongly. Overlap is what
  turns a difference into a hazard, because a shared index is the one thing a renumbering would have
  moved.
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
  the rest, and they all yield: no renumbering invents or removes a window function. The
  counterfactual is a real re-run rather than a list of type-dependent guards, so it cannot go stale
  when the next one is added.

None of this changes a verdict, since the pair is refused either way; it decides only which fact
the caller is handed first, and so which bucket the row is counted in.

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
* This is not hypothetical. The check was added after a real rewrite pair came out `provable`: one
  side carried more placeholders than the other, the two shared the lowest indices, and the shared
  `$1` stood for a different application value on each side. Such a pair now reports
  `parameter-misaligned: arity` and is never lowered. `tests/pairs/params/arity_misaligned.sql`
  pins that refusal on a small pair of the same kind, with a witness that separates its two sides.

### What the check costs, and what it does not

The check only ever refuses, so it can move a pair out of the emitted set and never into it. Three
properties follow, and they are arguments rather than counts:

* **It is raised last.** A construct the frontend cannot lower still outranks a misalignment, so the
  bucket holds pairs nothing else would have refused: it measures the exposure of this gap rather
  than relabelling refusals that were already there.
* **It needs no `sqleq-fuzz` cross-check.** It changes no lowering, only withholds one, so nothing
  new reaches the prover.
* **`order` firing rarely is not evidence of absence.** It sees only a permutation that leaves role
  evidence on both sides. A permutation between columns of one type, in positions the walk cannot
  tell apart, leaves none, which is the hole above.

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
know. One refinement that would keep those benign rows is tempting and wrong: "the orphan indices
all sit above every shared index, so dropping them cannot have renumbered the shared ones" also
readmits the false proof above, whose orphans all sit above the shared indices. It is exactly wrong
on the one case that matters. `order` can likewise fire on a legitimate rewrite that moves a
comparison onto a join partner (`a.id = $1 AND a.id = b.a_id` against `b.a_id = $1` has genuinely
disjoint role sets). Both lose proofs; neither invents one.

`parameter-misaligned` is deliberately **not** a claim of non-equivalence, even though a renumbered
pair usually is non-equivalent under index binding. The benign rows above are the counterexample to
that shortcut. Misalignment is evidence about the *question*, not about the answer, so it gets its
own reason and its own bucket in every harness that reports refusals.

## Reading further

* [VALIDATION.md](VALIDATION.md) — how the tool is validated, and the defects it has caught.
* [DESIGN.md](DESIGN.md) — why the frontend is built this way.
