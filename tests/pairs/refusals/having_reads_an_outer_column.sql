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
-- origin: a HAVING that reads a column of the outer query is refused, not lowered
--   (docs/SOUNDNESS.md); a pin for each documented refusal (#68)
-- witness: t = {(1, 0, 2)}; u = {(1, 0, 1), (2, 0, 2)}: the group's max(u.b) is 2 and its min 1, so
--   A returns 1 and B no rows
create table "t" ("id" INTEGER, "a" INTEGER, "b" INTEGER);
create table "u" ("id" INTEGER, "a" INTEGER, "b" INTEGER);
SELECT id FROM t WHERE EXISTS (SELECT 1 FROM u GROUP BY u.a HAVING max(u.b) = t.b);
SELECT id FROM t WHERE EXISTS (SELECT 1 FROM u GROUP BY u.a HAVING min(u.b) = t.b);
