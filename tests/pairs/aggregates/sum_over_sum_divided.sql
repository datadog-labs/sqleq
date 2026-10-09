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
-- origin: issue #121: a sum over a sum of integers (a bigint) is a numeric, a sum over the same cast to integer a bigint
-- witness: t = {(1, 1)}: A yields 0.50000000000000000000, B yields 0
create table "t" ("id" INTEGER, "a" INTEGER);
SELECT sum("s") / 2 FROM (SELECT sum("a") AS "s" FROM "t" GROUP BY "id") AS "x";
SELECT sum("s") / 2 FROM (SELECT CAST(sum("a") AS INTEGER) AS "s" FROM "t" GROUP BY "id") AS "x";
