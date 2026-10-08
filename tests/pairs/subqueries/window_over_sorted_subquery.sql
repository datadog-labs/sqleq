-- Unless explicitly stated otherwise all files in this repository are licensed under the
-- Apache License Version 2.0.
-- This product includes software developed at Datadog (https://www.datadoghq.com/).
-- Copyright 2026-Present Datadog, Inc.

-- truth: not-equivalent
-- expect frontend: refuse:unsupported
-- expect fuzz: nondet-skip
-- expect qed: no-plan
-- expect sqleq-solver: no-plan
-- expect lean: unsupported
-- origin: issue #59: a window function numbers the rows in the order a sorted subquery hands them on, and the
--   reflexivity check stripped that ORDER BY too
-- witness: t = {(1, 'b'), (2, 'a'), (3, 'c')}: on Postgres 17, A returns (1, a), (2, b), (3, c) and B (1, c), (2, b), (3, a)
create table "t" ("id" INTEGER, "x" TEXT);
SELECT row_number() OVER (), "x" FROM (SELECT "x" FROM "t" ORDER BY "x") AS "s";
SELECT row_number() OVER (), "x" FROM (SELECT "x" FROM "t" ORDER BY "x" DESC) AS "s";
