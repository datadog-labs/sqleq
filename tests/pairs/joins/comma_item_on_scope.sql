-- Unless explicitly stated otherwise all files in this repository are licensed under the
-- Apache License Version 2.0.
-- This product includes software developed at Datadog (https://www.datadoghq.com/).
-- Copyright 2026-Present Datadog, Inc.

-- truth: not-equivalent
-- expect frontend: emit
-- expect fuzz: no-counterexample
-- expect qed: no-proof
-- expect sqleq-solver: unsupported
-- expect lean: unsupported
-- origin: issue #47: the ON condition of a join after a comma item is resolved against the
--   earlier comma items too, so the bare x binds to a.x; Postgres hides a there and binds x to o.x
-- witness: o = {(1, 7)}, a = {(10, 5)}, b = {(20)}, c = {(30, 5)}: A returns no rows, B returns 1
create table "o" ("id" INTEGER, "x" INTEGER);
create table "a" ("id" INTEGER, "x" INTEGER);
create table "b" ("id" INTEGER);
create table "c" ("id" INTEGER, "z" INTEGER);
SELECT "o"."id" FROM "o" WHERE EXISTS (SELECT 1 FROM "a", "b" JOIN "c" ON "c"."z" = "x");
SELECT "o"."id" FROM "o" WHERE EXISTS (SELECT 1 FROM "a", "b" CROSS JOIN "c" WHERE "c"."z" = "a"."x");
