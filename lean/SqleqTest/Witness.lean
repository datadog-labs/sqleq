-- Unless explicitly stated otherwise all files in this repository are licensed under the
-- Apache License Version 2.0.
-- This product includes software developed at Datadog (https://www.datadoghq.com/).
-- Copyright 2026-Present Datadog, Inc.

import Sqleq

/-!
# Witness controls

Each line pins what the canonical run on an empty table does with one construct, so that a change
to the model that alters any of them fails here.
-/

namespace Sqleq.WitnessControls

open Sqleq

/-- Columns `(id, name, n)`; `id` is the key. -/
def plain : Col := ⟨true, .null, false⟩
def notNull : Col := ⟨false, .null, false⟩
def serial : Col := ⟨false, .fresh, false⟩

def two (rows : List (List Cell)) : Insert := ⟨0, [1, 2], .values rows, []⟩
def dup : List (List Cell) := [[.p 1, .p 2], [.p 1, .p 2]]
def distinct2 : List (List Cell) := [[.p 1, .p 2], [.p 3, .p 4]]

/-- Table `(id serial, name, n)`, unique on `name` (column 1); the insert lists `name, n`. -/
def byName (c : Conflict) : Spec := ⟨[serial, plain, plain], [⟨[1], false⟩], [1, 2], c⟩

-- Distinct rows insert.
example : witness (byName .none) (two distinct2) = .ok 2 := by decide +kernel
-- The same tuple twice collides on the key...
example : witness (byName .none) (two dup) = .unique 0 := by decide +kernel
-- ...which `DO NOTHING` skips, with or without an arbiter...
example : witness (byName .nothing) (two dup) = .ok 1 := by decide +kernel
example : witness (byName (.nothingOn 0)) (two dup) = .ok 1 := by decide +kernel
-- ...and `DO UPDATE` cannot: the colliding row was inserted by this same statement.
example : witness (byName (.update 0)) (two dup) = .secondTime 0 := by decide +kernel
-- A target no constraint matches is an error before any row.
example : witness (byName .noArbiter) (two distinct2) = .noArbiter := by decide +kernel

/-- Unique on the omitted serial `id` only: the same tuple twice gets two fresh ids. -/
def byId : Spec := ⟨[serial, plain, plain], [⟨[0], false⟩], [1, 2], .none⟩
example : witness byId (two dup) = .ok 2 := by decide +kernel

/-- An omitted NOT NULL column with no default fails every row. -/
def omitsNotNull : Spec := ⟨[notNull, plain, plain], [], [1, 2], .none⟩
example : witness omitsNotNull (two distinct2) = .notNull 0 := by decide +kernel

/-- A `NULL` cell in a NOT NULL column. -/
def listed : Spec := ⟨[serial, notNull, plain], [], [1, 2], .none⟩
example : witness listed (two [[.null, .p 2]]) = .notNull 1 := by decide +kernel

/-- NULL keys: distinct by default, colliding under `NULLS NOT DISTINCT`. -/
def nullKey (nnd : Bool) : Spec := ⟨[serial, plain, plain], [⟨[1], nnd⟩], [1, 2], .none⟩
def nulls : List (List Cell) := [[.null, .p 2], [.null, .p 4]]
example : witness (nullKey false) (two nulls) = .ok 2 := by decide +kernel
example : witness (nullKey true) (two nulls) = .unique 0 := by decide +kernel

/-- A once-per-statement default in the key: two rows get the same value and collide. -/
def sameInKey : Spec := ⟨[⟨false, .same, false⟩, plain, plain], [⟨[0], false⟩], [1, 2], .none⟩
example : witness sameInKey (two distinct2) = .unique 0 := by decide +kernel

/-- A `GENERATED ALWAYS` column in the insert list. -/
def always : Spec := ⟨[serial, ⟨true, .null, true⟩, plain], [], [1, 2], .none⟩
example : witness always (two distinct2) = .generated 1 := by decide +kernel

/-- A composite key collides only when every column matches. -/
def composite : Spec := ⟨[serial, plain, plain], [⟨[1, 2], false⟩], [1, 2], .none⟩
example : witness composite (two [[.p 1, .p 2], [.p 1, .p 4]]) = .ok 2 := by decide +kernel
example : witness composite (two dup) = .unique 0 := by decide +kernel

/-! ### Generated cells -/

/-- The insert lists all three columns of `(id, name, n)`. -/
def three (rows : List (List Cell)) : Insert := ⟨0, [0, 1, 2], .values rows, []⟩
def dfltRows (k : GenKind) : List (List Cell) := [[.gen k, .p 1, .p 2], [.gen k, .p 3, .p 4]]
/-- Unique on column 0, whose column is `c`. -/
def keyedBy (c : Col) : Spec := ⟨[c, plain, plain], [⟨[0], false⟩], [0, 1, 2], .none⟩

-- `DEFAULT` on a serial key gives each row its own id...
example : witness (keyedBy serial) (three (dfltRows .dflt)) = .ok 2 := by decide +kernel
-- ...on a once-per-statement default, the two rows collide...
example : witness (keyedBy ⟨false, .same, false⟩) (three (dfltRows .dflt)) = .unique 0 := by
  decide +kernel
-- ...and on a NOT NULL column with no default, it is NULL.
example : witness (keyedBy notNull) (three (dfltRows .dflt)) = .notNull 0 := by decide +kernel
-- A generator with a new value per row does not collide; one that may repeat does.
example : witness (keyedBy plain) (three (dfltRows .fresh)) = .ok 2 := by decide +kernel
example : witness (keyedBy plain) (three (dfltRows .once)) = .unique 0 := by decide +kernel
-- `DEFAULT` in a `GENERATED ALWAYS` column is legal in Postgres, but the column is still listed;
-- the model keeps refusing it, as a backstop behind the translator's own refusal.
example : witness (keyedBy ⟨false, .fresh, true⟩) (three (dfltRows .dflt)) = .generated 0 := by
  decide +kernel

/-- A fresh generated cell and an omitted serial draw distinct values from the same counter. -/
def freshPair : Spec := ⟨[serial, plain, plain], [⟨[0], false⟩, ⟨[1], false⟩], [1, 2], .none⟩
example : witness freshPair (two [[.gen .fresh, .p 1], [.gen .fresh, .p 2]]) = .ok 2 := by
  decide +kernel

end Sqleq.WitnessControls
