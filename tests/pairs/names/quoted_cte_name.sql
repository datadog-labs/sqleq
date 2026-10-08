-- Unless explicitly stated otherwise all files in this repository are licensed under the
-- Apache License Version 2.0.
-- This product includes software developed at Datadog (https://www.datadoghq.com/).
-- Copyright 2026-Present Datadog, Inc.

-- truth: not-equivalent
-- expect frontend: emit
-- expect fuzz: counterexample
-- expect qed: no-proof
-- expect sqleq-solver: no-proof
-- expect lean: unsupported
-- origin: issue #57: inline_ctes folded the CTE name "T" to t, so it replaced the base table t
-- witness: t = {(1, 2), (2, 1)}: A returns 1 and 2, B returns 2 and 3

create table "t" ("a" INTEGER, "b" INTEGER);
WITH "T" AS (SELECT "a" + 1 AS "a" FROM "t") SELECT "a" FROM t;
SELECT "a" + 1 AS "a" FROM "t";
