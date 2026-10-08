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
-- origin: issue #88: Postgres names a VALUES column column1, which the frontend left unnamed, so a bare
--   column1 missed the derived table and resolved to the enclosing u.column1
-- witness: u = {(1, 1)}: A yields no rows (s.column1 is 2), B yields 1
create table "u" ("id" INTEGER, "column1" INTEGER);
SELECT "id" FROM "u" WHERE EXISTS (SELECT 1 FROM (VALUES (2)) AS "s" WHERE "column1" = 1);
SELECT "id" FROM "u" WHERE EXISTS (SELECT 1 FROM (VALUES (2)) AS "s" WHERE "u"."column1" = 1);
