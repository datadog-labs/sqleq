-- Unless explicitly stated otherwise all files in this repository are licensed under the
-- Apache License Version 2.0.
-- This product includes software developed at Datadog (https://www.datadoghq.com/).
-- Copyright 2026-Present Datadog, Inc.

-- truth: not-equivalent
-- expect frontend: refuse:unsupported
-- expect fuzz: no-tables
-- expect qed: no-plan
-- expect sqleq-solver: no-plan
-- expect lean: unsupported
-- origin: issue #49: random_normal() was missing from the frontend's volatile list, so the WITH
--   binding read twice was inlined and lowered, and both sides became the same IR
-- witness: any database: A returns true, B returns false except with probability zero
create table "t" ("id" INTEGER);
WITH "c" AS (SELECT random_normal() AS "r") SELECT "x"."r" = "y"."r" FROM "c" AS "x", "c" AS "y";
SELECT "x"."r" = "y"."r" FROM (SELECT random_normal() AS "r") AS "x", (SELECT random_normal() AS "r") AS "y";
