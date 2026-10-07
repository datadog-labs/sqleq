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
-- origin: issue #85: regr_avgx is declared over double precision only, so it converts its integer arguments and adds them in floating point
-- witness: t = {(9007199254740993), (1)}, u = {(-9007199254740992)}, rows in that order: A yields 0, B yields 0.3333333333333333
create table "t" ("x" BIGINT);
create table "u" ("x" BIGINT);
SELECT regr_avgx("x", "x") FROM (SELECT "x" FROM "t" UNION ALL SELECT "x" FROM "u") AS "s";
SELECT regr_avgx("x", "x") FROM (SELECT "x" FROM "u" UNION ALL SELECT "x" FROM "t") AS "s";
