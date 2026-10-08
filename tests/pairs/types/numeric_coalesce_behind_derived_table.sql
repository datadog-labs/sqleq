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
-- origin: issue #84: coalesce() typed its numeric result opaque, so a cast to text one query block up did not see a numeric
-- witness: t = {(2.0)}, u = {(2.00)}: A yields '2.0', B yields '2.00'
create table "t" ("x" NUMERIC);
create table "u" ("x" NUMERIC);
SELECT CAST("a"."c" AS TEXT) FROM (SELECT coalesce("x", 0) AS "c" FROM "t") AS "a" JOIN (SELECT coalesce("x", 0) AS "c" FROM "u") AS "b" ON "a"."c" = "b"."c";
SELECT CAST("b"."c" AS TEXT) FROM (SELECT coalesce("x", 0) AS "c" FROM "t") AS "a" JOIN (SELECT coalesce("x", 0) AS "c" FROM "u") AS "b" ON "a"."c" = "b"."c";
