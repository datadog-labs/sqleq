-- Unless explicitly stated otherwise all files in this repository are licensed under the
-- Apache License Version 2.0.
-- This product includes software developed at Datadog (https://www.datadoghq.com/).
-- Copyright 2026-Present Datadog, Inc.

-- truth: equivalent
-- expect frontend: emit
-- expect fuzz: no-counterexample
-- expect qed: proved
-- expect sqleq-solver: proved
-- expect lean: unsupported
-- origin: issue #47: the control beside joins/comma_item_right_join.sql; each comma item is now its
--   own join tree, which for an inner join is only a regrouping
-- argument: the inner join's condition names only b and c, and an inner join reassociates with the
--   cross join, so a CROSS JOIN (b JOIN c ON p) and (a CROSS JOIN b) JOIN c ON p are the same rows
create table "a" ("id" INTEGER, "x" INTEGER);
create table "b" ("id" INTEGER, "x" INTEGER);
create table "c" ("id" INTEGER, "x" INTEGER);
SELECT "a"."id", "c"."id" FROM "a", "b" JOIN "c" ON "b"."x" = "c"."x";
SELECT "a"."id", "c"."id" FROM "a" CROSS JOIN "b" JOIN "c" ON "b"."x" = "c"."x";
