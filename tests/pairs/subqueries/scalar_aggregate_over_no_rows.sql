-- Unless explicitly stated otherwise all files in this repository are licensed under the
-- Apache License Version 2.0.
-- This product includes software developed at Datadog (https://www.datadoghq.com/).
-- Copyright 2026-Present Datadog, Inc.

-- truth: not-equivalent
-- expect frontend: emit
-- expect fuzz: no-counterexample
-- expect qed: no-proof
-- expect sqleq-solver: no-proof
-- expect lean: unsupported
-- origin: issue #60: a scalar aggregate subquery always has its one row, and sqleq-solver read it as never NULL
-- witness: t = {(1)}, s = {}: sum over no rows is NULL, so A returns nothing and B returns 1
create table "t" ("id" INTEGER);
create table "s" ("k" INTEGER, "y" INTEGER);
SELECT "id" FROM "t" WHERE (SELECT sum("y") FROM "s") IS NOT NULL;
SELECT "id" FROM "t";
