-- Unless explicitly stated otherwise all files in this repository are licensed under the
-- Apache License Version 2.0.
-- This product includes software developed at Datadog (https://www.datadoghq.com/).
-- Copyright 2026-Present Datadog, Inc.

-- truth: not-equivalent
-- expect frontend: emit
-- expect fuzz: no-counterexample
-- expect qed: no-proof
-- expect sqleq-solver: unsupported
-- expect lean: unsupported
-- origin: issue #57: strip_identical_pagination matched any key's text against an alias's, so the
--   key t.a, the input column a, counted as the output column "t.a" and the pagination was dropped
-- witness: t = {(1, 2), (2, 1)}: A returns 2 (the b of the row with the least a), B returns 1 (the
--   least b, which its t.a is)

create table "t" ("a" INTEGER, "b" INTEGER);
SELECT "b" AS "t.a" FROM "t" ORDER BY t.a LIMIT 1;
SELECT "b" AS "t.a" FROM (SELECT "b", "b" AS "a" FROM "t") AS "t" ORDER BY t.a LIMIT 1;
