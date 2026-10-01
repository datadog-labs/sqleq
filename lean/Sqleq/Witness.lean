-- Unless explicitly stated otherwise all files in this repository are licensed under the
-- Apache License Version 2.0.
-- This product includes software developed at Datadog (https://www.datadoghq.com/).
-- Copyright 2026-Present Datadog, Inc.

import Sqleq.Source

/-!
# Non-vacuity witnesses

`EquivGather` says the two sides agree under every binding. A pair whose `VALUES` side *always
errors* satisfies that trivially. Two identical rows under a unique key, or an omitted NOT NULL
column with no default, make every run fail the same way on both sides. So a proof is credited
only with a **witness**: the `VALUES` side succeeding, and inserting at least one row, on one
concrete run.

## The run

The table starts **empty**. The binding is **canonical**: every parameter gets its own non-NULL
value, `WVal.param n`, distinct from every other parameter's and from every default's.

Canonical is the strongest choice. With distinct non-NULL parameters, two rows collide on a key
only where the statement forces it: the same parameter in both, or the same once-per-statement
default. Those collide under every binding, so if the canonical run fails, every run with non-NULL
parameters fails, and none of them is a witness.

## The model

- **Defaults**:
  - `null`: no default, so NULL;
  - `same`: one value for every row of the statement. That covers a constant, a clock such as
    `now()`, and any default the translator does not recognise, the choice that maximises
    collisions;
  - `fresh`: a new value per row, as a sequence or `gen_random_uuid()` gives.
- **NOT NULL** is checked before the conflict clause, as Postgres does.
- **Unique constraints**: two rows collide when every key column is non-NULL and equal, or, under
  `NULLS NOT DISTINCT`, equal treating NULLs as equal.
- **`ON CONFLICT`**:
  - `DO NOTHING` with no target skips on any collision;
  - with an arbiter, it skips on the arbiter and raises on any other collision;
  - `DO UPDATE` on an empty table can only collide with a row this same statement inserted, which
    Postgres rejects ("cannot affect row a second time");
  - a target that no constraint matches is an error before any row is read.
- **`GENERATED ALWAYS` columns** in the insert list are an error.

CHECK and foreign-key constraints are not modelled. A witness here says nothing about them, and
the Postgres replay is what checks them.

This model is only ever used for non-vacuity. A bug in it can wrongly credit an always-failing
pair, which the replay exists to catch, or wrongly withhold credit. It cannot make
`EquivGather` false.
-/

namespace Sqleq

/-- How an omitted column is filled. -/
inductive Dflt where
  | null
  | same
  | fresh
  deriving DecidableEq, Repr

structure Col where
  nullable : Bool
  dflt : Dflt
  /-- `GENERATED ALWAYS`: an explicit value is an error. -/
  always : Bool
  deriving Repr

structure Uniq where
  /-- Key columns, as table column indices. -/
  cols : List Nat
  /-- `NULLS NOT DISTINCT` -/
  nnd : Bool
  deriving Repr

inductive Conflict where
  /-- No `ON CONFLICT` clause. -/
  | none
  /-- `ON CONFLICT DO NOTHING` with no target: any collision skips the row. -/
  | nothing
  /-- `ON CONFLICT (…) DO NOTHING`, arbitrated by unique constraint `u`. -/
  | nothingOn (u : Nat)
  /-- `ON CONFLICT (…) DO UPDATE …`, arbitrated by unique constraint `u`. -/
  | update (u : Nat)
  /-- A conflict target no unique constraint matches: Postgres raises before reading a row. -/
  | noArbiter
  deriving Repr

structure Spec where
  /-- The target table's columns, in table order. -/
  cols : List Col
  uniques : List Uniq
  /-- The insert's column list, as table column indices. -/
  ins : List Nat
  conflict : Conflict
  deriving Repr

/-- A value in the canonical run. -/
inductive WVal where
  | param (n : Nat)
  | same (col : Nat)
  | fresh (i : Nat)
  deriving DecidableEq, Repr

abbrev Row := List (Option WVal)

/-- How the canonical run ends. -/
inductive WResult where
  | ok (inserted : Nat)
  | notNull (col : Nat)
  | unique (u : Nat)
  | secondTime (u : Nat)
  | generated (col : Nat)
  | noArbiter
  | nothingInserted
  deriving DecidableEq, Repr

def WResult.isOk : WResult → Bool
  | .ok _ => true
  | _ => false

/-- Position of `i` in `xs`, counting from `p`. -/
def pos : List Nat → Nat → Nat → Option Nat
  | [], _, _ => none
  | x :: xs, i, p => if x == i then some p else pos xs i (p + 1)

def cellW : Cell → Option WVal
  | .p n => some (.param n)
  | .pc n _ => some (.param n)
  | .null => none

/-- The full table row for one `VALUES` row, starting at table column `i`, with `fr` the next
fresh value. Returns the row and the next fresh value. -/
def fullRow (ins : List Nat) (row : List Cell) : Nat → List Col → Nat → Row × Nat
  | _, [], fr => ([], fr)
  | i, c :: cs, fr =>
    match pos ins i 0 with
    | some p =>
      let rest := fullRow ins row (i + 1) cs fr
      (cellW (row.getD p .null) :: rest.1, rest.2)
    | none =>
      match c.dflt with
      | .null =>
        let rest := fullRow ins row (i + 1) cs fr
        (none :: rest.1, rest.2)
      | .same =>
        let rest := fullRow ins row (i + 1) cs fr
        (some (.same i) :: rest.1, rest.2)
      | .fresh =>
        let rest := fullRow ins row (i + 1) cs (fr + 1)
        (some (.fresh fr) :: rest.1, rest.2)

/-- The first NOT NULL column holding NULL. -/
def notNullAt : Nat → List Col → Row → Option Nat
  | i, c :: cs, v :: vs => if !c.nullable && v.isNone then some i else notNullAt (i + 1) cs vs
  | _, _, _ => none

/-- The first `GENERATED ALWAYS` column the insert lists. -/
def generatedAt (ins : List Nat) : Nat → List Col → Option Nat
  | _, [] => none
  | i, c :: cs => if c.always && (pos ins i 0).isSome then some i else generatedAt ins (i + 1) cs

def keyEq (u : Uniq) (a b : Row) : Bool :=
  u.cols.all fun c =>
    match a.getD c none, b.getD c none with
    | some x, some y => x == y
    | none, none => u.nnd
    | _, _ => false

/-- Indices of the unique constraints `row` collides on, among the rows already inserted. -/
def hits (st : List Row) (row : Row) : Nat → List Uniq → List Nat
  | _, [] => []
  | i, u :: us =>
    if st.any (keyEq u row) then i :: hits st row (i + 1) us else hits st row (i + 1) us

/-- One row of the canonical run: insert it, skip it, or stop with an error. -/
def step (sp : Spec) (st : List Row) (row : Row) : Except WResult (List Row) :=
  match notNullAt 0 sp.cols row with
  | some c => .error (.notNull c)
  | none =>
    match hits st row 0 sp.uniques with
    | [] => .ok (row :: st)
    | h :: hs =>
      match sp.conflict with
      | .nothing => .ok st
      | .nothingOn a => if (h :: hs).contains a then .ok st else .error (.unique h)
      | .update a => if (h :: hs).contains a then .error (.secondTime a) else .error (.unique h)
      | .none | .noArbiter => .error (.unique h)

def runRows (sp : Spec) : List (List Cell) → Nat → List Row → WResult
  | [], _, st => if st.isEmpty then .nothingInserted else .ok st.length
  | r :: rs, fr, st =>
    let built := fullRow sp.ins r 0 sp.cols fr
    match step sp st built.1 with
    | .error e => e
    | .ok st' => runRows sp rs built.2 st'

/-- The canonical run of an `INSERT … VALUES` on an empty table. -/
def witness (sp : Spec) (a : Insert) : WResult :=
  match sp.conflict with
  | .noArbiter => .noArbiter
  | _ =>
    match generatedAt sp.ins 0 sp.cols with
    | some c => .generated c
    | none =>
      match a.src with
      | .values rows => runRows sp rows 0 []
      | .unnest _ => .nothingInserted

end Sqleq
