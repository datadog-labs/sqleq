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
-- origin: issue #108: the result of round(integer, integer), a numeric, was typed VARBINARY and read as a type whose = is identity
-- witness: t = {(1, 1)}: x = 1.0 and y = 1.00, so x = y holds; A yields 1.0 and B yields 1.00
create table "t" ("id" INTEGER, "i" INTEGER);
SELECT CAST("x" AS TEXT) FROM (SELECT round("i", 1) AS "x", round("i", 2) AS "y" FROM "t") AS "s" WHERE "x" = "y";
SELECT CAST("y" AS TEXT) FROM (SELECT round("i", 1) AS "x", round("i", 2) AS "y" FROM "t") AS "s" WHERE "x" = "y";
