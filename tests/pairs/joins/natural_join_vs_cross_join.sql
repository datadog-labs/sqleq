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
-- origin: NATURAL JOIN is refused (`join_op`); read as a join with no constraint, it would be the
--   CROSS JOIN beside it. No test reached that refusal until this pair
-- witness: t = {(1, 1)}, u = {(2, 2)}: no row of t agrees with one of u on a, so A returns none;
--   B returns 1
create table "t" ("a" INTEGER, "b" INTEGER);
create table "u" ("a" INTEGER, "c" INTEGER);
SELECT "b" FROM "t" NATURAL JOIN "u";
SELECT "b" FROM "t" CROSS JOIN "u";
