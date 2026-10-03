-- Unless explicitly stated otherwise all files in this repository are licensed under the
-- Apache License Version 2.0.
-- This product includes software developed at Datadog (https://www.datadoghq.com/).
-- Copyright 2026-Present Datadog, Inc.

import Sqleq.Source

/-!
# The gather checker

`checkGather tys a b` is the per-pair computation the kernel runs. When it returns `true`, the
theorem `checkGather_sound` (in `Sqleq.Gather`) turns that into `EquivGather a b`. For a pair whose
`VALUES` side has generated cells, `checkGatherGen` and `EquivGatherGen` at the end of this file
play the same parts.

Most of what it checks is the *modelling* preconditions, not the semantics. Values are abstract
in this model, so a mismatched cast type would not make the Lean statement false; it would make the
statement *about the wrong thing*. With `$j::T[]` where `T` is not the column's type, Postgres
applies an assignment coercion the two sides do not share (`varchar(10)[]` truncates where
`VALUES` raises). Requiring equal types is what makes "the parameter's value is the column's value"
true on both sides.

`tys` lists the declared *base* type id of each target column, in column-list order, interned by
the translator with aliases resolved. A modifier on the column (`varchar(10)`) is allowed, because
both sides reach it through the same assignment coercion from the base type. A cast carrying a
modifier, or an array-typed column, never reaches here.
-/

namespace Sqleq

/-- The unnest arguments are exactly `$i::tᵢ[], $(i+1)::tᵢ₊₁[], …`, one per column, each element
type equal to its column's type. -/
def argsOk : Nat → List Nat → List Arg → Bool
  | _, [], [] => true
  | i, t :: ts, a :: as => a.param == i && a.ty == t && argsOk (i + 1) ts as
  | _, _, _ => false

/-- `$n` sits in column `j` of `k`, and only there: `(n - 1) mod k = j`.

This is a modelling precondition, not something the proof needs. Postgres gives an untyped
parameter the type of the column it is assigned to, so a `$n` in two columns of different types
fails type inference and the whole statement errors, which the model would not see. Pinning each
parameter to one column rules that out cheaply. Both shapes that matter satisfy it: the same tuple
repeated (`$j` is always in column `j`) and row-major numbering (`$(r·k + j + 1)` is in column
`j`). -/
def slotOk (k j n : Nat) : Bool :=
  1 ≤ n && (n - 1) % k == j

/-- A `VALUES` cell in column `j` of `k`, whose declared type is `t`: `$n` or `$n::t` with the
parameter pinned to this column, or `NULL`.

A generated cell is refused. It has no parameter to gather, and `cellVal` reads it as NULL, so
without this arm the claim would be about a statement that inserts NULL where the real one inserts
a default. Only `checkGatherGen` admits one, under its own claim. -/
def cellOk (k j t : Nat) : Cell → Bool
  | .p n => slotOk k j n
  | .pc n c => c == t && slotOk k j n
  | .null => true
  | .gen _ => false

/-- A `VALUES` row has exactly one admissible cell per column. `j` counts the columns seen. -/
def rowOk (k : Nat) : Nat → List Nat → List Cell → Bool
  | _, [], [] => true
  | j, t :: ts, c :: cs => cellOk k j t c && rowOk k (j + 1) ts cs
  | _, _, _ => false

def rowsOk (tys : List Nat) : List (List Cell) → Bool
  | [] => true
  | r :: rs => rowOk tys.length 0 tys r && rowsOk tys rs

/-- The tail mentions no parameter. On the `VALUES` side `$k+1` is a cell; on the unnest side the
same `$k+1` could be a scalar in `DO UPDATE SET`, so a shared tail is only shared if it is
parameter-free. -/
def tailOk : List Tok → Bool
  | [] => true
  | .param _ :: _ => false
  | .word _ :: ts => tailOk ts

/-- A is `VALUES`, B is `unnest`, and everything but the source is literally the same. -/
def checkGather (tys : List Nat) (a b : Insert) : Bool :=
  match a.src, b.src with
  | .values rows@(_ :: _), .unnest args@(_ :: _) =>
      a.target == b.target && a.cols == b.cols && a.tail == b.tail && tailOk a.tail &&
      tys.length == a.cols.length && argsOk 1 tys args && rowsOk tys rows
  | _, _ => false

/-- **What `proved-gather` claims.** For every value type, every binding of A's scalar
parameters, and every way an INSERT could turn (target, columns, tail, row sequence) into a
result, B run under the gather binding produces the same result as A.

`run` is universally quantified, so the claim holds however `ON CONFLICT`, defaults, NOT NULL,
unique, CHECK and foreign-key checks, sequences, clocks and triggers behave. The one assumption is
that an INSERT's effect depends only on those four things, which the Postgres oracle tests. -/
def EquivGather (a b : Insert) : Prop :=
  match a.src, b.src with
  | .values rows, .unnest args =>
      ∀ (V R : Type) (run : Nat → List Nat → List Tok → List (List (Option V)) → R)
        (β : Scalars V),
        run b.target b.cols b.tail (unnestRows (gather rows β) args) =
          run a.target a.cols a.tail (valuesRows β rows)
  | _, _ => False

/-! ## Generated cells

`checkGatherGen` admits `VALUES` cells the database fills in (`DEFAULT`, `now()`, `nextval('s')`)
where the unnest side supplies that column's array itself. Its claim, `EquivGatherGen`, is weaker
than `EquivGather`: B reproduces A when B's arrays hold the values A's generated cells evaluated
to. -/

/-- A cell's type, for the generated-cell checker: a cast must be to the column's own type. Where
a parameter may sit is checked apart, by `pinSeq`. -/
def cellOkG (t : Nat) : Cell → Bool
  | .pc _ c => c == t
  | _ => true

def rowOkG : List Nat → List Cell → Bool
  | [], [] => true
  | t :: ts, c :: cs => cellOkG t c && rowOkG ts cs
  | _, _ => false

def rowsOkG (tys : List Nat) : List (List Cell) → Bool
  | [] => true
  | r :: rs => rowOkG tys r && rowsOkG tys rs

/-- Reading one row, where the next parameter cell must be `$nx`: the number expected after the
row, or `0` once a cell breaks the order. `NULL` and generated cells take no number. -/
def seqRow : Nat → List Cell → Nat
  | nx, [] => nx
  | nx, .p n :: cs => if nx != 0 && n == nx then seqRow (nx + 1) cs else 0
  | nx, .pc n _ :: cs => if nx != 0 && n == nx then seqRow (nx + 1) cs else 0
  | nx, _ :: cs => seqRow nx cs

def seqRows : Nat → List (List Cell) → Nat
  | nx, [] => nx
  | nx, r :: rs => seqRows (seqRow nx r) rs

/-- The parameters are `$1, $2, …` in reading order, each exactly once: row-major numbering that
skips `NULL` and generated cells.

A modelling precondition, like `slotOk`. A parameter used once sits in one column, so Postgres
types it from that column. With no gaps, no parameter is left untyped, which would fail type
inference before any row is read. The slot rule does not carry over, because a generated cell
takes a column but no number. -/
def pinSeq (rows : List (List Cell)) : Bool :=
  seqRows 1 rows != 0

def hasGenRow : List Cell → Bool
  | [] => false
  | .gen _ :: _ => true
  | _ :: cs => hasGenRow cs

/-- Some cell is generated. This makes "`proved-gather-generated` ⇒ A really has a generated
cell" something the kernel checks. -/
def hasGen : List (List Cell) → Bool
  | [] => false
  | r :: rs => hasGenRow r || hasGen rs

/-- As `checkGather`, with generated cells admitted and parameters numbered by `pinSeq`. -/
def checkGatherGen (tys : List Nat) (a b : Insert) : Bool :=
  match a.src, b.src with
  | .values rows@(_ :: _), .unnest args@(_ :: _) =>
      a.target == b.target && a.cols == b.cols && a.tail == b.tail && tailOk a.tail &&
      tys.length == a.cols.length && argsOk 1 tys args && rowsOkG tys rows && pinSeq rows &&
      hasGen rows
  | _, _ => false

/-- **What `proved-gather-generated` claims.** For every value type, every binding of A's scalar
parameters, every value `g` A's generated cells could evaluate to, and every `run`, B under the
gather binding, with each generated cell's entry taken from `g`, produces the same result as A
whose generated cells evaluate to `g`.

So B reproduces A when it is given A's generated values. Nothing here says where B would get
them. For a sequence or an identity, that is the caller supplying ids, which leaves the sequence
behind them: what a generator does besides produce its value is outside the claim. -/
def EquivGatherGen (a b : Insert) : Prop :=
  match a.src, b.src with
  | .values rows, .unnest args =>
      ∀ (V R : Type) (run : Nat → List Nat → List Tok → List (List (Option V)) → R)
        (β : Scalars V) (g : Generated V),
        run b.target b.cols b.tail (unnestRows (gatherG rows β g) args) =
          run a.target a.cols a.tail (valuesRowsG β g rows)
  | _, _ => False

end Sqleq
