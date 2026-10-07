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
-- origin: issue #85: a double precision SUM over a sorted subquery against the same SUM in scan order
-- witness: t = {(1, 1e20), (3, 1), (2, -1e20)}, stored in that order: A sums in y order and yields 1, B sums in scan order and yields 0
create table "t" ("y" INTEGER, "x" DOUBLE PRECISION);
SELECT SUM("x") FROM (SELECT "x" FROM "t" ORDER BY "y") AS "s";
SELECT SUM("x") FROM "t";
