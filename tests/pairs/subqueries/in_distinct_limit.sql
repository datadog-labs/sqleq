-- Unless explicitly stated otherwise all files in this repository are licensed under the
-- Apache License Version 2.0.
-- This product includes software developed at Datadog (https://www.datadoghq.com/).
-- Copyright 2026-Present Datadog, Inc.

-- truth: not-equivalent
-- expect frontend: emit
-- expect fuzz: counterexample
-- expect qed: no-proof
-- expect sqleq-solver: unsupported
-- expect sqlsolver-jvm: no-proof
-- expect lean: unsupported
-- origin: DISTINCT inside an IN subquery was stripped even under LIMIT, where it decides which rows the LIMIT keeps
-- witness: u = {1, 1, 2}, t = {(1, 2, NULL)}: A's subquery is {1, 2} and keeps the row, B's is {1, 1} and drops it
create table "t" ("id" INTEGER, "a" INTEGER, "b" INTEGER, unique ("id"));
create table "u" ("x" INTEGER);
SELECT "id" FROM "t" WHERE "a" IN (SELECT DISTINCT "x" FROM "u" ORDER BY "x" LIMIT 2);
SELECT "id" FROM "t" WHERE "a" IN (SELECT "x" FROM "u" ORDER BY "x" LIMIT 2);
