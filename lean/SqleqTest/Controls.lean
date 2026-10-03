-- Unless explicitly stated otherwise all files in this repository are licensed under the
-- Apache License Version 2.0.
-- This product includes software developed at Datadog (https://www.datadoghq.com/).
-- Copyright 2026-Present Datadog, Inc.

import Sqleq

/-!
# Checker controls

Each positive control must pass `checkGather`; each negative control must fail it. Where a negative
control is refused because the pair is *genuinely* not equivalent (not merely outside the fragment),
that is shown too, by a kernel-checked counterexample to `EquivGather`: `run` returns the rows
themselves, and `β n = some n` gives every parameter a distinct value.

A checker that accepted everything would fail this file, and so would one that refused everything.
-/

namespace Sqleq.Controls

open Sqleq

/-- `run` that exposes the row sequence, so two different sequences give different results. -/
def expose : Nat → List Nat → List Tok → List (List (Option Nat)) → List (List (Option Nat)) :=
  fun _ _ _ rows => rows

def distinct : Scalars Nat := fun n => some n

/-- A counterexample to `EquivGather` for `VALUES rows` vs `unnest args`. -/
theorem not_equiv_of_rows {ta tb : Nat} {ca cb : List Nat} {tla tlb : List Tok}
    {rows : List (List Cell)} {args : List Arg}
    (h : unnestRows (gather rows distinct) args ≠ valuesRows distinct rows) :
    ¬ EquivGather ⟨ta, ca, .values rows, tla⟩ ⟨tb, cb, .unnest args, tlb⟩ := by
  intro heq
  exact h (heq Nat _ expose distinct)

def tys2 : List Nat := [3, 4]
def cols2 : List Nat := [10, 11]
def tail0 : List Tok := [.word 7, .word 8]

/-- `INSERT INTO t (c1, c2) VALUES ($1, $2), ($3, $4)`. -/
def rowMajor : Insert := ⟨1, cols2, .values [[.p 1, .p 2], [.p 3, .p 4]], tail0⟩
/-- `INSERT INTO t (c1, c2) VALUES ($1, $2), ($1, $2)`: the same tuple twice. -/
def repeated : Insert := ⟨1, cols2, .values [[.p 1, .p 2], [.p 1, .p 2]], tail0⟩
/-- `INSERT INTO t (c1, c2) SELECT * FROM unnest($1::t3[], $2::t4[])`. -/
def unnest2 : Insert := ⟨1, cols2, .unnest [⟨1, 3⟩, ⟨2, 4⟩], tail0⟩

/-! ### Positive controls -/

example : checkGather tys2 rowMajor unnest2 = true := by decide +kernel
example : checkGather tys2 repeated unnest2 = true := by decide +kernel
/-- Casts to the column's own type are admitted, as is `NULL`. -/
example : checkGather tys2 ⟨1, cols2, .values [[.pc 1 3, .null], [.p 3, .pc 4 4]], tail0⟩ unnest2 = true := by
  decide +kernel

/-! ### Negative controls -/

/-- The unnest element type is not the column's type (`$2::t5[]` into a `t4` column). -/
example : checkGather tys2 rowMajor ⟨1, cols2, .unnest [⟨1, 3⟩, ⟨2, 5⟩], tail0⟩ = false := by
  decide +kernel

/-- A `VALUES` cell cast to a type that is not its column's. -/
example : checkGather tys2 ⟨1, cols2, .values [[.pc 1 9, .p 2]], tail0⟩ unnest2 = false := by
  decide +kernel

/-- One parameter in two columns, `VALUES ($1, $1)`. Postgres types an untyped parameter from its
column, so with two column types this statement fails type inference; the model cannot see that,
so the checker refuses it. -/
example : checkGather tys2 ⟨1, cols2, .values [[.p 1, .p 1]], tail0⟩ unnest2 = false := by
  decide +kernel

/-- A parameter numbered for another column, `VALUES ($2, $1)`. -/
example : checkGather tys2 ⟨1, cols2, .values [[.p 2, .p 1]], tail0⟩ unnest2 = false := by
  decide +kernel

/-- `$0` is not a Postgres parameter. -/
example : checkGather tys2 ⟨1, cols2, .values [[.p 0, .p 2]], tail0⟩ unnest2 = false := by
  decide +kernel

/-- A parameter in the shared tail, as in `DO UPDATE SET c2 = $3`. -/
example : checkGather tys2 ⟨1, cols2, rowMajor.src, [.word 7, .param 3]⟩
    ⟨1, cols2, unnest2.src, [.word 7, .param 3]⟩ = false := by decide +kernel

/-- Different tails. -/
example : checkGather tys2 rowMajor ⟨1, cols2, unnest2.src, [.word 7]⟩ = false := by decide +kernel

/-- Different target tables. -/
example : checkGather tys2 rowMajor ⟨2, cols2, unnest2.src, tail0⟩ = false := by decide +kernel

/-- A reordered column list on one side. -/
example : checkGather tys2 rowMajor ⟨1, [11, 10], unnest2.src, tail0⟩ = false := by decide +kernel

/-- B := A: both sides `VALUES`. An emitter that copied A into B must not get a proof. -/
example : checkGather tys2 rowMajor rowMajor = false := by decide +kernel

/-- Both sides unnest. -/
example : checkGather tys2 unnest2 unnest2 = false := by decide +kernel

/-- A row narrower than the column list. -/
example : checkGather tys2 ⟨1, cols2, .values [[.p 1, .p 2], [.p 3]], tail0⟩ unnest2 = false := by
  decide +kernel

/-- Fewer unnest arguments than columns. -/
example : checkGather tys2 rowMajor ⟨1, cols2, .unnest [⟨1, 3⟩], tail0⟩ = false := by decide +kernel

/-- **Swapped unnest arguments**, `unnest($2::t4[], $1::t3[])`. Refused, and genuinely not
equivalent under the gather rule: array `$1` is column 1, so unnest yields each row reversed. -/
def swapped : Insert := ⟨1, cols2, .unnest [⟨2, 4⟩, ⟨1, 3⟩], tail0⟩

example : checkGather tys2 rowMajor swapped = false := by decide +kernel
example : ¬ EquivGather rowMajor swapped := not_equiv_of_rows (by decide)

/-- **Unequal array lengths are NULL-padded**, so a pair whose `VALUES` rows are ragged is not
the unnest of its columns. Refused, and genuinely not equivalent. -/
def ragged : Insert := ⟨1, cols2, .values [[.p 1, .p 2], [.p 3]], tail0⟩

example : ¬ EquivGather ragged unnest2 := not_equiv_of_rows (by decide)

/-! ### Generated cells

The same discipline for `checkGatherGen`. Its counterexamples also give every generated cell its
own value, `g i j`, distinct from every parameter's. -/

def distinctG : Generated Nat := fun i j => some (1000000 + 1000 * i + j)

/-- A counterexample to `EquivGatherGen` for `VALUES rows` vs `unnest args`. -/
theorem not_equivGen_of_rows {ta tb : Nat} {ca cb : List Nat} {tla tlb : List Tok}
    {rows : List (List Cell)} {args : List Arg}
    (h : unnestRows (gatherG rows distinct distinctG) args ≠ valuesRowsG distinct distinctG rows) :
    ¬ EquivGatherGen ⟨ta, ca, .values rows, tla⟩ ⟨tb, cb, .unnest args, tlb⟩ := by
  intro heq
  exact h (heq Nat _ expose distinct distinctG)

/-- `INSERT INTO t (c1, c2) VALUES (DEFAULT, $1), (DEFAULT, $2)`. -/
def dfltCol : Insert := ⟨1, cols2, .values [[.gen .dflt, .p 1], [.gen .dflt, .p 2]], tail0⟩
/-- A column mixing generated and parameter cells: `VALUES (DEFAULT, $1), ($2, $3)`. -/
def mixed : Insert := ⟨1, cols2, .values [[.gen .dflt, .p 1], [.p 2, .p 3]], tail0⟩

example : checkGatherGen tys2 dfltCol unnest2 = true := by decide +kernel
example : checkGatherGen tys2 mixed unnest2 = true := by decide +kernel
/-- Every generator kind, a cast to the column's type, and `NULL`, which takes no number. -/
example : checkGatherGen tys2
    ⟨1, cols2, .values [[.gen .fresh, .pc 1 4], [.null, .gen .once], [.p 2, .null]], tail0⟩
    unnest2 = true := by decide +kernel

/-- `checkGather` refuses a generated cell: its claim has no value for one. -/
example : checkGather tys2 dfltCol unnest2 = false := by decide +kernel
example : checkGather tys2 ⟨1, cols2, .values [[.pc 1 3, .gen .once]], tail0⟩ unnest2 = false := by
  decide +kernel

/-- A pair with no generated cell is `checkGather`'s, not this checker's. -/
example : checkGatherGen tys2 rowMajor unnest2 = false := by decide +kernel

/-- Out of reading order, `VALUES (DEFAULT, $2), (DEFAULT, $1)`. The gather rule goes by position,
so this pair is still equivalent; it is refused as a modelling precondition, like `($2, $1)`. -/
example : checkGatherGen tys2 ⟨1, cols2, .values [[.gen .dflt, .p 2], [.gen .dflt, .p 1]], tail0⟩
    unnest2 = false := by decide +kernel

/-- A gap, `VALUES (DEFAULT, $1), (DEFAULT, $3)`: nothing types `$2`, so Postgres rejects the
statement before reading a row. -/
example : checkGatherGen tys2 ⟨1, cols2, .values [[.gen .dflt, .p 1], [.gen .dflt, .p 3]], tail0⟩
    unnest2 = false := by decide +kernel

/-- One parameter twice, `VALUES (DEFAULT, $1), (DEFAULT, $1)`. -/
example : checkGatherGen tys2 ⟨1, cols2, .values [[.gen .dflt, .p 1], [.gen .dflt, .p 1]], tail0⟩
    unnest2 = false := by decide +kernel

/-- `$0` is not a Postgres parameter. -/
example : checkGatherGen tys2 ⟨1, cols2, .values [[.gen .dflt, .p 0]], tail0⟩ unnest2 = false := by
  decide +kernel

/-- A cast to a type that is not the column's. -/
example : checkGatherGen tys2 ⟨1, cols2, .values [[.gen .dflt, .pc 1 9]], tail0⟩ unnest2 = false := by
  decide +kernel

/-- A parameter in the shared tail. -/
example : checkGatherGen tys2 ⟨1, cols2, dfltCol.src, [.word 7, .param 3]⟩
    ⟨1, cols2, unnest2.src, [.word 7, .param 3]⟩ = false := by decide +kernel

/-- A row narrower than the column list. -/
example : checkGatherGen tys2 ⟨1, cols2, .values [[.gen .dflt, .p 1], [.p 2]], tail0⟩
    unnest2 = false := by decide +kernel

/-- **Swapped unnest arguments** with a generated column: refused, and genuinely not equivalent. -/
example : checkGatherGen tys2 dfltCol swapped = false := by decide +kernel
example : ¬ EquivGatherGen dfltCol swapped := not_equivGen_of_rows (by decide)

/-- **Ragged rows** with a generated column: genuinely not equivalent. -/
example : ¬ EquivGatherGen ⟨1, cols2, .values [[.gen .dflt, .p 1], [.p 2]], tail0⟩ unnest2 :=
  not_equivGen_of_rows (by decide)

end Sqleq.Controls
