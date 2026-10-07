-- Unless explicitly stated otherwise all files in this repository are licensed under the
-- Apache License Version 2.0.
-- This product includes software developed at Datadog (https://www.datadoghq.com/).
-- Copyright 2026-Present Datadog, Inc.

-- truth: not-equivalent
-- expect frontend: refuse:unsupported
-- expect fuzz: no-counterexample
-- expect qed: no-plan
-- expect sqleq-solver: no-plan
-- expect lean: unsupported
-- origin: two tables whose names differ only in case is refused, not lowered (docs/SOUNDNESS.md); a
--   pin for each documented refusal (#68)
-- witness: t = {(1)}; "T" = {}: A returns 1, B no rows
create table "t" ("id" INTEGER);
create table "T" ("id" INTEGER);
SELECT id FROM "t";
SELECT id FROM "T";
