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
-- origin: issue #57: a derived table's output names "b" and "B" were both lower-cased, and a
--   reference to either resolved to the first
-- witness: t = {(1, 2)}: A returns 2, B returns 1

create table "t" ("a" INTEGER, "b" INTEGER);
SELECT "s"."B" FROM (SELECT "a" AS "b", "b" AS "B" FROM "t") AS "s";
SELECT "s"."b" FROM (SELECT "a" AS "b", "b" AS "B" FROM "t") AS "s";
