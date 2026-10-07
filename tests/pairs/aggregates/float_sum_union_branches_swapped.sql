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
-- origin: issue #85: double precision SUM depends on the order its inputs arrive in, and UNION ALL in the other order feeds the same bag in another order
-- witness: t = {(1e20), (1)}, u = {(-1e20)}, rows in that order: A yields 0, B yields 1
create table "t" ("x" DOUBLE PRECISION);
create table "u" ("x" DOUBLE PRECISION);
SELECT SUM("x") FROM (SELECT "x" FROM "t" UNION ALL SELECT "x" FROM "u") AS "s";
SELECT SUM("x") FROM (SELECT "x" FROM "u" UNION ALL SELECT "x" FROM "t") AS "s";
