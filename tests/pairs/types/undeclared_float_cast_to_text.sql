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
-- origin: issue #108: the result of sqrt(integer), a double precision, was typed VARBINARY and read as a type whose = is identity
-- witness: t = {(1, 0, 0)}: x = -0 and y = 0, so x = y holds; A yields -0 and B yields 0
create table "t" ("id" INTEGER, "i" INTEGER, "j" INTEGER);
SELECT CAST("x" AS TEXT) FROM (SELECT -sqrt("i") AS "x", sqrt("j") AS "y" FROM "t") AS "s" WHERE "x" = "y";
SELECT CAST("y" AS TEXT) FROM (SELECT -sqrt("i") AS "x", sqrt("j") AS "y" FROM "t") AS "s" WHERE "x" = "y";
