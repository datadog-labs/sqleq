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
-- origin: issue #85: a double precision SUM over a sorted subquery depends on the sort, and the frontend drops both ORDER BYs
-- witness: t = {(1, 1e20), (2, -1e20), (3, 1)}: A sums 1e20, -1e20, 1 and yields 1; B sums 1, -1e20, 1e20 and yields 0
-- The SUM is refused, so lowering no longer makes the two sides one plan. Until issue #59 the reflexivity
-- check still made them one query: it stripped a subquery's ORDER BY that no row slice reads.
create table "t" ("y" INTEGER, "x" DOUBLE PRECISION);
SELECT SUM("x") FROM (SELECT "x" FROM "t" ORDER BY "y") AS "s";
SELECT SUM("x") FROM (SELECT "x" FROM "t" ORDER BY "y" DESC) AS "s";
