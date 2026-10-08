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
-- origin: issue #108: a subscript of an opaque value was read as a value whose = is identity, though p[0] over a point is a double precision
-- witness: t = {(1, '(-0,0)', '(0,0)')}: x = -0 and y = 0, so x = y holds; A yields -0 and B yields 0
create table "t" ("id" INTEGER, "p" point, "q" point);
SELECT CAST("x" AS TEXT) FROM (SELECT "p"[0] AS "x", "q"[0] AS "y" FROM "t") AS "s" WHERE "x" = "y";
SELECT CAST("y" AS TEXT) FROM (SELECT "p"[0] AS "x", "q"[0] AS "y" FROM "t") AS "s" WHERE "x" = "y";
