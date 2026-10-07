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
-- origin: issue #60: sqleq-solver read a scalar subquery as NULL only when it returns no row, so one row holding NULL was not NULL
-- witness: t = {(1)}, s = {(1, NULL)}: the subquery returns one row, whose value is NULL, so A returns 1 and B returns nothing
create table "t" ("id" INTEGER);
create table "s" ("k" INTEGER, "y" INTEGER, unique ("k"));
SELECT "id" FROM "t" WHERE (SELECT "y" FROM "s" WHERE "k" = 1) IS NULL;
SELECT "id" FROM "t" WHERE NOT EXISTS (SELECT "y" FROM "s" WHERE "k" = 1);
