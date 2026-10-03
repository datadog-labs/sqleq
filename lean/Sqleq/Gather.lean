-- Unless explicitly stated otherwise all files in this repository are licensed under the
-- Apache License Version 2.0.
-- This product includes software developed at Datadog (https://www.datadoghq.com/).
-- Copyright 2026-Present Datadog, Inc.

import Sqleq.Check

/-!
# Soundness of the gather checker

`checkGather_sound`: if the kernel computes `checkGather tys a b = true`, then `EquivGather a b`.
`checkGatherGen_sound` is the same for generated cells: `checkGatherGen` gives `EquivGatherGen`.

The whole argument is the **gather lemma**, `unnestRows_gather`: unnest over the gathered arrays
yields exactly the `VALUES` rows, row for row and cell for cell. After that the two statements feed
the same row sequence to the same (target, columns, tail), so any `run` gives the same result.
-/

namespace Sqleq

variable {V : Type}

/-! ### What the checker's parts guarantee -/

theorem argsOk_spec : ∀ (i : Nat) (tys : List Nat) (args : List Arg),
    argsOk i tys args = true →
      args.length = tys.length ∧ ∀ j (h : j < args.length), (args[j]'h).param = i + j
  | _, [], [], _ => ⟨rfl, fun _ h => absurd h (Nat.not_lt_zero _)⟩
  | i, t :: ts, a :: as, h => by
    simp only [argsOk, Bool.and_eq_true, beq_iff_eq] at h
    obtain ⟨⟨hp, _⟩, hrest⟩ := h
    obtain ⟨hlen, hidx⟩ := argsOk_spec (i + 1) ts as hrest
    refine ⟨by simp [hlen], fun j hj => ?_⟩
    cases j with
    | zero => simp [hp]
    | succ j =>
      have := hidx j (by simpa using hj)
      simp only [List.getElem_cons_succ, this]
      omega
  | _, [], _ :: _, h => by simp [argsOk] at h
  | _, _ :: _, [], h => by simp [argsOk] at h

theorem rowOk_length (k : Nat) : ∀ (j : Nat) (tys : List Nat) (r : List Cell),
    rowOk k j tys r = true → r.length = tys.length
  | _, [], [], _ => rfl
  | j, _ :: ts, _ :: cs, h => by
    simp only [rowOk, Bool.and_eq_true] at h
    simp [rowOk_length k (j + 1) ts cs h.2]
  | _, [], _ :: _, h => by simp [rowOk] at h
  | _, _ :: _, [], h => by simp [rowOk] at h

theorem rowsOk_length (tys : List Nat) : ∀ (rows : List (List Cell)),
    rowsOk tys rows = true → ∀ r ∈ rows, r.length = tys.length
  | [], _ => by simp
  | r :: rs, h => by
    simp only [rowsOk, Bool.and_eq_true] at h
    intro r' hr'
    rcases List.mem_cons.mp hr' with rfl | hr'
    · exact rowOk_length tys.length 0 tys r' h.1
    · exact rowsOk_length tys rs h.2 r' hr'

theorem tailOk_spec : ∀ t : List Tok, tailOk t = true → ∀ n, Tok.param n ∉ t
  | [], _ => by simp
  | .param _ :: _, h => by simp [tailOk] at h
  | .word _ :: ts, h => by
    intro n hn
    rcases List.mem_cons.mp hn with h' | h'
    · cases h'
    · exact tailOk_spec ts h n h'

/-! ### `zipPad` over arrays of one length -/

theorem maxLen_const (n : Nat) : ∀ (arrays : List (List (Option V))),
    arrays ≠ [] → (∀ a ∈ arrays, a.length = n) → maxLen arrays = n
  | [], h, _ => absurd rfl h
  | [a], _, h => by simp [maxLen, h a (by simp)]
  | a :: b :: rest, _, h => by
    have ha := h a (by simp)
    have hrest := maxLen_const n (b :: rest) (by simp) (fun x hx => h x (List.mem_cons_of_mem _ hx))
    simp only [maxLen] at hrest ⊢
    rw [ha, hrest, Nat.max_self]

/-! ### The gather lemma -/

theorem getD_of_lt {α : Type} (l : List α) (i : Nat) (d : α) (h : i < l.length) :
    l.getD i d = l[i] := by
  simp [List.getD_eq_getElem?_getD, List.getElem?_eq_getElem h]

/-- Unnest over the gathered arrays yields exactly the `VALUES` rows. The preconditions are what
`checkGather` computes: the arguments are `$1..$k` in order, every row has `k` cells, and there is
at least one argument and at least one row. -/
theorem unnestRows_gather (β : Scalars V) (tys : List Nat) (rows : List (List Cell))
    (args : List Arg) (hargs : argsOk 1 tys args = true) (hrows : rowsOk tys rows = true)
    (hne : args ≠ []) :
    unnestRows (gather rows β) args = valuesRows β rows := by
  obtain ⟨hlen, hidx⟩ := argsOk_spec 1 tys args hargs
  have hwidth := rowsOk_length tys rows hrows
  -- Every gathered array is one column, so it has one element per row.
  have hmax : maxLen (args.map fun a => gather rows β a.param) = rows.length := by
    apply maxLen_const
    · simpa using hne
    · intro x hx
      obtain ⟨a, _, rfl⟩ := List.mem_map.mp hx
      simp [gather]
  unfold unnestRows zipPad valuesRows
  rw [hmax]
  apply List.ext_getElem
  · simp
  · intro i h1 h2
    have hi : i < rows.length := by simpa using h1
    have hri : rows[i].length = args.length := by rw [hlen]; exact hwidth _ (List.getElem_mem hi)
    simp only [List.getElem_map, List.getElem_range]
    apply List.ext_getElem
    · simp [hri]
    · intro j hj1 hj2
      have hj : j < args.length := by simpa using hj1
      simp only [List.getElem_map]
      rw [getD_of_lt _ _ _ (by simp [gather]; exact hi)]
      simp only [gather, List.getElem_map, hidx j hj]
      rw [getD_of_lt _ _ _ (by rw [hri]; omega)]
      congr 2
      omega

/-! ### Soundness -/

theorem checkGather_sound (tys : List Nat) (a b : Insert) (h : checkGather tys a b = true) :
    EquivGather a b := by
  cases a with
  | mk ta ac asrc atail =>
  cases b with
  | mk bt bc bsrc btail =>
  cases asrc with
  | unnest _ => simp [checkGather] at h
  | values rows =>
  cases bsrc with
  | values _ => cases rows <;> simp [checkGather] at h
  | unnest args =>
  cases rows with
  | nil => simp [checkGather] at h
  | cons r rs =>
  cases args with
  | nil => simp [checkGather] at h
  | cons x xs =>
  simp only [checkGather, Bool.and_eq_true, beq_iff_eq] at h
  obtain ⟨⟨⟨⟨⟨⟨ht, hc⟩, htail⟩, _⟩, _⟩, hargs⟩, hrows⟩ := h
  intro V R run β
  simp only
  rw [unnestRows_gather β tys (r :: rs) (x :: xs) hargs hrows (by simp), ht, hc, htail]

/-! ### Generated cells

The same argument, with each row and cell carrying its position so that a generated cell reads its
value from `g`. `pinSeq` and `hasGen` are modelling preconditions; the proof does not use them. -/

theorem rowOkG_length : ∀ (tys : List Nat) (r : List Cell),
    rowOkG tys r = true → r.length = tys.length
  | [], [], _ => rfl
  | _ :: ts, _ :: cs, h => by
    simp only [rowOkG, Bool.and_eq_true] at h
    simp [rowOkG_length ts cs h.2]
  | [], _ :: _, h => by simp [rowOkG] at h
  | _ :: _, [], h => by simp [rowOkG] at h

theorem rowsOkG_length (tys : List Nat) : ∀ (rows : List (List Cell)),
    rowsOkG tys rows = true → ∀ r ∈ rows, r.length = tys.length
  | [], _ => by simp
  | r :: rs, h => by
    simp only [rowsOkG, Bool.and_eq_true] at h
    intro r' hr'
    rcases List.mem_cons.mp hr' with rfl | hr'
    · exact rowOkG_length tys r' h.1
    · exact rowsOkG_length tys rs h.2 r' hr'

/-- The gather lemma with generated cells: unnest over the gathered arrays yields exactly the rows
`VALUES` produces when its generated cells evaluate to `g`. -/
theorem unnestRows_gatherG (β : Scalars V) (g : Generated V) (tys : List Nat)
    (rows : List (List Cell)) (args : List Arg) (hargs : argsOk 1 tys args = true)
    (hrows : rowsOkG tys rows = true) (hne : args ≠ []) :
    unnestRows (gatherG rows β g) args = valuesRowsG β g rows := by
  obtain ⟨hlen, hidx⟩ := argsOk_spec 1 tys args hargs
  have hwidth := rowsOkG_length tys rows hrows
  have hmax : maxLen (args.map fun a => gatherG rows β g a.param) = rows.length := by
    apply maxLen_const
    · simpa using hne
    · intro x hx
      obtain ⟨a, _, rfl⟩ := List.mem_map.mp hx
      simp [gatherG]
  unfold unnestRows zipPad valuesRowsG
  rw [hmax]
  apply List.ext_getElem
  · simp
  · intro i h1 h2
    have hi : i < rows.length := by simpa using h1
    have hri : rows[i].length = args.length := by rw [hlen]; exact hwidth _ (List.getElem_mem hi)
    simp only [List.getElem_map, List.getElem_range, List.getElem_mapIdx]
    apply List.ext_getElem
    · simp [hri]
    · intro j hj1 hj2
      have hj : j < args.length := by simpa using hj1
      simp only [List.getElem_map, List.getElem_mapIdx]
      rw [getD_of_lt _ _ _ (by simp [gatherG]; exact hi)]
      simp only [gatherG, List.getElem_mapIdx, hidx j hj]
      have hj' : 1 + j - 1 = j := by omega
      simp only [hj']
      rw [getD_of_lt _ _ _ (by rw [hri]; exact hj)]

theorem checkGatherGen_sound (tys : List Nat) (a b : Insert) (h : checkGatherGen tys a b = true) :
    EquivGatherGen a b := by
  cases a with
  | mk ta ac asrc atail =>
  cases b with
  | mk bt bc bsrc btail =>
  cases asrc with
  | unnest _ => simp [checkGatherGen] at h
  | values rows =>
  cases bsrc with
  | values _ => cases rows <;> simp [checkGatherGen] at h
  | unnest args =>
  cases rows with
  | nil => simp [checkGatherGen] at h
  | cons r rs =>
  cases args with
  | nil => simp [checkGatherGen] at h
  | cons x xs =>
  simp only [checkGatherGen, Bool.and_eq_true, beq_iff_eq] at h
  obtain ⟨⟨⟨⟨⟨⟨⟨⟨ht, hc⟩, htail⟩, _⟩, _⟩, hargs⟩, hrows⟩, _⟩, _⟩ := h
  intro V R run β g
  simp only
  rw [unnestRows_gatherG β g tys (r :: rs) (x :: xs) hargs hrows (by simp), ht, hc, htail]

end Sqleq
