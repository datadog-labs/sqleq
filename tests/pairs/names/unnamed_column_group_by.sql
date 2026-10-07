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
-- origin: issue #88: a GROUP BY name that missed the derived table's unnamed CASE column (Postgres's
--   "case") fell back to the select-list alias "case", a constant, instead of the input column
-- witness: t = {(-1), (1)}: A groups by s."case" and yields (1, 1) twice, B yields (1, 2)
create table "t" ("v" INTEGER);
SELECT 1 AS "case", count(*) FROM (SELECT CASE WHEN "v" > 0 THEN 1 ELSE 0 END FROM "t") AS "s" GROUP BY "case";
SELECT 1 AS "case", count(*) FROM (SELECT CASE WHEN "v" > 0 THEN 1 ELSE 0 END FROM "t") AS "s" GROUP BY 2 - 1;
