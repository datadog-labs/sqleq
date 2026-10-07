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
-- origin: issue #50: lowering ignored a WITH RECURSIVE that inline_ctes leaves in place, so the
--   binding's name resolved to the base table it shadows
-- witness: t = {(7)}: A returns 1, 2, 3; B returns 7
create table "t" ("a" INTEGER);
WITH RECURSIVE "t" ("a") AS (SELECT 1 UNION ALL SELECT "a" + 1 FROM "t" WHERE "a" < 3) SELECT "a" FROM "t";
SELECT "a" FROM "t";
