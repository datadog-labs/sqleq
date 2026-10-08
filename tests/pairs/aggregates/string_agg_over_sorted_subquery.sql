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
-- origin: issue #59: the reflexivity check stripped the ORDER BY of a sorted subquery feeding string_agg,
--   so ASC and DESC normalized to one query; the order a sorted subquery hands on is observable
-- witness: t = {(1, 'b'), (2, 'a'), (3, 'c')}: on Postgres 17, A returns 'a,b,c' and B 'c,b,a'
create table "t" ("id" INTEGER, "x" TEXT);
SELECT string_agg("s"."x", ',') FROM (SELECT "x" FROM "t" ORDER BY "x" ASC) AS "s";
SELECT string_agg("s"."x", ',') FROM (SELECT "x" FROM "t" ORDER BY "x" DESC) AS "s";
