-- Unless explicitly stated otherwise all files in this repository are licensed under the
-- Apache License Version 2.0.
-- This product includes software developed at Datadog (https://www.datadoghq.com/).
-- Copyright 2026-Present Datadog, Inc.

-- truth: not-equivalent
-- expect frontend: refuse:unsupported
-- expect fuzz: not-comparable
-- expect qed: no-plan
-- expect sqleq-solver: no-plan
-- expect lean: unsupported
-- origin: issue #84: a UNION ALL column takes its first branch's type, so a numeric in the second branch was read as an integer
-- witness: t = {(5, 2.0)}, u = {(6, 2.00)}: A yields '2.0', B yields '2.00'
create table "t" ("i" INTEGER, "n" NUMERIC);
create table "u" ("i" INTEGER, "n" NUMERIC);
SELECT CAST("a"."c" AS TEXT) FROM (SELECT "i" AS "c" FROM "t" UNION ALL SELECT "n" FROM "t") AS "a" JOIN (SELECT "i" AS "c" FROM "u" UNION ALL SELECT "n" FROM "u") AS "b" ON "a"."c" = "b"."c";
SELECT CAST("b"."c" AS TEXT) FROM (SELECT "i" AS "c" FROM "t" UNION ALL SELECT "n" FROM "t") AS "a" JOIN (SELECT "i" AS "c" FROM "u" UNION ALL SELECT "n" FROM "u") AS "b" ON "a"."c" = "b"."c";
