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
-- origin: issue #88: Postgres names an unaliased CASE "case", which the frontend left unnamed, so a
--   bare "case" missed the derived table and resolved to the enclosing u."case"
-- witness: u = {(1, 1)}, t = {(-1)}: A yields no rows (s."case" is 0), B yields 1
create table "t" ("v" INTEGER);
create table "u" ("id" INTEGER, "case" INTEGER);
SELECT "id" FROM "u" WHERE EXISTS (SELECT 1 FROM (SELECT CASE WHEN "v" > 0 THEN 1 ELSE 0 END FROM "t") AS "s" WHERE "case" = 1);
SELECT "id" FROM "u" WHERE EXISTS (SELECT 1 FROM (SELECT CASE WHEN "v" > 0 THEN 1 ELSE 0 END FROM "t") AS "s" WHERE "u"."case" = 1);
