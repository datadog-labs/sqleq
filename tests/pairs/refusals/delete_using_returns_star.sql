-- Unless explicitly stated otherwise all files in this repository are licensed under the
-- Apache License Version 2.0.
-- This product includes software developed at Datadog (https://www.datadoghq.com/).
-- Copyright 2026-Present Datadog, Inc.

-- truth: not-equivalent
-- expect frontend: refuse:unsupported
-- expect fuzz: counterexample
-- expect qed: no-plan
-- expect sqleq-solver: no-plan
-- expect lean: unsupported
-- origin: a DELETE ... USING with RETURNING * is refused, not lowered (docs/SOUNDNESS.md); a pin
--   for each documented refusal (#68)
-- witness: t = {(1, 1)}; u = {(5, 1)}: both delete the row; A returns (1, 1, 5, 1), B returns (1,
--   1)
create table "t" ("id" INTEGER, "a" INTEGER);
create table "u" ("id" INTEGER, "a" INTEGER);
DELETE FROM t USING u WHERE t.a = u.a RETURNING *;
DELETE FROM t WHERE EXISTS (SELECT 1 FROM u WHERE t.a = u.a) RETURNING *;
