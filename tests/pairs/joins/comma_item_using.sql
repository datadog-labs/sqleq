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
-- origin: issue #47: in FROM a, b JOIN c USING (x) the USING name is looked up on every earlier
--   FROM item and found on a first; Postgres joins b and c only, on b.x = c.x
-- witness: a = {(1, 1)}, b = {(2, 2)}, c = {(3, 1)}: A returns no rows, B returns (1, 2, 3)
create table "a" ("id" INTEGER, "x" INTEGER);
create table "b" ("id" INTEGER, "x" INTEGER);
create table "c" ("id" INTEGER, "x" INTEGER);
SELECT "a"."id", "b"."id", "c"."id" FROM "a", "b" JOIN "c" USING ("x");
SELECT "a"."id", "b"."id", "c"."id" FROM "a" JOIN "c" USING ("x"), "b";
