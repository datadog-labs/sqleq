-- Unless explicitly stated otherwise all files in this repository are licensed under the
-- Apache License Version 2.0.
-- This product includes software developed at Datadog (https://www.datadoghq.com/).
-- Copyright 2026-Present Datadog, Inc.

-- truth: not-equivalent
-- expect frontend: refuse:unsupported
-- expect fuzz: nondet-skip
-- expect qed: no-plan
-- expect sqleq-solver: no-plan
-- expect lean: unsupported
-- origin: issue #49: the reflexivity check inlined a WITH binding read twice, duplicating the
--   random() call in it; Postgres evaluates the binding once
-- witness: any database: A returns true (one random() value read twice), B returns false (two
--   independent values) except with probability zero
create table "t" ("id" INTEGER);
WITH "c" AS (SELECT random() AS "r") SELECT "x"."r" = "y"."r" FROM "c" AS "x", "c" AS "y";
SELECT "x"."r" = "y"."r" FROM (SELECT random() AS "r") AS "x", (SELECT random() AS "r") AS "y";
