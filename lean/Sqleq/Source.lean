-- Unless explicitly stated otherwise all files in this repository are licensed under the
-- Apache License Version 2.0.
-- This product includes software developed at Datadog (https://www.datadoghq.com/).
-- Copyright 2026-Present Datadog, Inc.

/-!
# INSERT sources as row sequences

The syntax the translator emits, and what an INSERT's source denotes: the *sequence* of rows it
feeds to the insert. Order is kept on purpose. Serial ids, which duplicate `ON CONFLICT DO NOTHING`
keeps, and `RETURNING` order all depend on it, so a bag here would be unsound.

Every name (table, column, type, tail token) is interned to a `Nat` by the translator. The kernel
then compares numbers, never strings, and every function is structurally recursive so that
`decide +kernel` can evaluate it.

Values are abstract: `V` is any type, and `Option V` is a value that may be NULL. Nothing here
interprets a value, which is what lets a proof hold for every column type at once.
-/

namespace Sqleq

/-- What fills a generated `VALUES` cell. Only the witness reads the kind; the equivalence claim
holds for every value a generated cell could take. -/
inductive GenKind where
  /-- `DEFAULT`: the column's own default. -/
  | dflt
  /-- A generator with a new value per row: `nextval('s')`, `gen_random_uuid()`. -/
  | fresh
  /-- A generator not known to differ between rows: `now()`, `current_timestamp`, and
  `clock_timestamp()`, which can repeat. -/
  | once
  deriving DecidableEq, Repr

/-- One cell of a `VALUES` row. There is no `Option` argument for the cast: measured, a `none` per
cell costs about a third of elaboration on a 60-row pair. -/
inductive Cell where
  /-- `$n` -/
  | p (n : Nat)
  /-- `$n::t`, where `t` is a type id -/
  | pc (n t : Nat)
  /-- `NULL` -/
  | null
  /-- A cell the database fills in: `DEFAULT`, or a generator call such as `now()`. Only
  `checkGatherGen` admits it. -/
  | gen (k : GenKind)
  deriving DecidableEq, Repr

/-- One argument of a multi-argument `unnest`: `$param::ty[]`, where `ty` is the element type. -/
structure Arg where
  param : Nat
  ty : Nat
  deriving DecidableEq, Repr

/-- The row source of an `INSERT`. -/
inductive Source where
  /-- `VALUES (…), (…)`, rows in order. -/
  | values (rows : List (List Cell))
  /-- `SELECT * FROM unnest($p₁::T₁[], …, $pₖ::Tₖ[])`. -/
  | unnest (args : List Arg)
  deriving Repr

/-- A token of the statement's tail: everything after the source (conflict clause, `RETURNING`),
in the translator's canonical rendering. A parameter stays visible as its own token so the
checker can see it. -/
inductive Tok where
  | word (n : Nat)
  | param (n : Nat)
  deriving DecidableEq, Repr

/-- An `INSERT INTO target (cols) <src> <tail>`. -/
structure Insert where
  target : Nat
  cols : List Nat
  src : Source
  tail : List Tok
  deriving Repr

variable {V : Type}

/-- A scalar binding gives each `$n` a value; an array binding gives each `$n` an array. -/
abbrev Scalars (V : Type) := Nat → Option V
abbrev Arrays (V : Type) := Nat → List (Option V)

/-- A cell's value under `β`. A generated cell has no value here; no pair `checkGather` accepts has
one. -/
def cellVal (β : Scalars V) : Cell → Option V
  | .p n => β n
  | .pc n _ => β n
  | .null => none
  | .gen _ => none

/-- The rows `VALUES` produces, in order. -/
def valuesRows (β : Scalars V) (rows : List (List Cell)) : List (List (Option V)) :=
  rows.map (·.map (cellVal β))

/-- The longest array's length. -/
def maxLen : List (List (Option V)) → Nat
  | [] => 0
  | a :: as => max a.length (maxLen as)

/-- Postgres's multi-argument `unnest` in `FROM`: row `i` holds element `i` of each array, and
arrays shorter than the longest are padded with NULL. -/
def zipPad (arrays : List (List (Option V))) : List (List (Option V)) :=
  (List.range (maxLen arrays)).map fun i => arrays.map fun a => a.getD i none

/-- The rows `SELECT * FROM unnest(…)` produces, in order. -/
def unnestRows (γ : Arrays V) (args : List Arg) : List (List (Option V)) :=
  zipPad (args.map fun a => γ a.param)

/-- **The gather rule.** The unnest side's array `$j` is column `j` of the `VALUES` rows, in row
order, evaluated under the scalar binding `β`. Column `j` is the (j-1)-th cell of each row. -/
def gather (rows : List (List Cell)) (β : Scalars V) : Arrays V :=
  fun p => rows.map fun r => cellVal β (r.getD (p - 1) .null)

/-! ### Generated cells

A generated cell's value is chosen by the database, not by a parameter. `Generated V` gives each
position a value: `g i j` is what the cell in row `i`, column `j` (both from 0) evaluated to.
Indexing by position, not by a name the translator assigns, means two cells can never be forced to
share a value. -/

abbrev Generated (V : Type) := Nat → Nat → Option V

/-- `cellVal`, with `v` the value the cell evaluated to if it is generated. -/
def cellValG (β : Scalars V) (v : Option V) : Cell → Option V
  | .p n => β n
  | .pc n _ => β n
  | .null => none
  | .gen _ => v

/-- The rows `VALUES` produces when its generated cells evaluate to `g`. -/
def valuesRowsG (β : Scalars V) (g : Generated V) (rows : List (List Cell)) :
    List (List (Option V)) :=
  rows.mapIdx fun i r => r.mapIdx fun j c => cellValG β (g i j) c

/-- **The gather rule with generated cells.** As `gather`, except that a generated cell
contributes the value it evaluated to: the unnest side's array `$j` holds column `j` of the rows
`VALUES` produced. -/
def gatherG (rows : List (List Cell)) (β : Scalars V) (g : Generated V) : Arrays V :=
  fun p => rows.mapIdx fun i r => cellValG β (g i (p - 1)) (r.getD (p - 1) .null)

end Sqleq
