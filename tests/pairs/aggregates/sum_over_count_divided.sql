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
-- origin: issue #121: count is a bigint, so a sum over it is a numeric and its division a numeric one
-- witness: t = {(1, 0)}: the one count is 1, so sum(c) / 2 = 0.5; A returns no row and B returns 1
create table "t" ("id" INTEGER, "a" INTEGER);
SELECT 1 FROM (SELECT count(*) AS "c" FROM "t" GROUP BY "id") AS "s" HAVING sum("c") / 2 = 0;
SELECT 1 FROM (SELECT count(*) AS "c" FROM "t" GROUP BY "id") AS "s" HAVING sum("c") / 2 < 1 AND sum("c") / 2 > -1;
