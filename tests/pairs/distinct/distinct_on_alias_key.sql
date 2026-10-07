-- Unless explicitly stated otherwise all files in this repository are licensed under the
-- Apache License Version 2.0.
-- This product includes software developed at Datadog (https://www.datadoghq.com/).
-- Copyright 2026-Present Datadog, Inc.

-- truth: not-equivalent
-- expect frontend: emit
-- expect fuzz: counterexample
-- expect qed: no-proof
-- expect sqleq-solver: unsupported
-- expect lean: unsupported
-- origin: issue #46: a DISTINCT ON key is resolved in the FROM scope only; Postgres resolves
--   it like an ORDER BY key, output names first, so A's key a is the output column a (t.b)
-- witness: t = {(1, 1), (2, 1)}: A returns one row, B returns two
create table "t" ("a" INTEGER, "b" INTEGER);
SELECT DISTINCT ON ("a") "a" AS "b", "b" AS "a" FROM "t";
SELECT DISTINCT ON ("t"."a") "a" AS "b", "b" AS "a" FROM "t";
