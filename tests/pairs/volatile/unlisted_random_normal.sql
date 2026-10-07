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
-- origin: issue #49: random_normal() is volatile but was missing from the frontend's list, so it
--   was an uninterpreted function and two calls were one value
-- witness: t = {(1)}: A returns two different numbers, B the same number twice
create table "t" ("id" INTEGER);
SELECT random_normal() AS "a", random_normal() AS "b" FROM "t";
SELECT "x"."r" AS "a", "x"."r" AS "b" FROM (SELECT random_normal() AS "r" FROM "t") AS "x";
