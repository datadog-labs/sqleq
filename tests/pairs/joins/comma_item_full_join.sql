-- Unless explicitly stated otherwise all files in this repository are licensed under the
-- Apache License Version 2.0.
-- This product includes software developed at Datadog (https://www.datadoghq.com/).
-- Copyright 2026-Present Datadog, Inc.

-- truth: not-equivalent
-- expect frontend: emit
-- expect fuzz: counterexample
-- expect qed: no-proof
-- expect sqleq-solver: no-proof
-- expect lean: unsupported
-- origin: issue #47: the comma-item grouping, with FULL JOIN
-- witness: a = {}, b = {(2, 1)}, c = {(3, 1)}: A returns no rows, B returns 3
create table "a" ("id" INTEGER, "x" INTEGER);
create table "b" ("id" INTEGER, "x" INTEGER);
create table "c" ("id" INTEGER, "x" INTEGER);
SELECT "c"."id" FROM "a", "b" FULL JOIN "c" ON "b"."x" = "c"."x";
SELECT "c"."id" FROM "a" CROSS JOIN "b" FULL JOIN "c" ON "b"."x" = "c"."x";
