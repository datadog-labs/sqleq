-- Unless explicitly stated otherwise all files in this repository are licensed under the
-- Apache License Version 2.0.
-- This product includes software developed at Datadog (https://www.datadoghq.com/).
-- Copyright 2026-Present Datadog, Inc.

-- truth: not-equivalent
-- expect frontend: emit
-- expect fuzz: counterexample
-- expect qed: no-proof
-- expect sqleq-solver: proved !known-unsound
-- expect lean: unsupported
-- origin: issue #65: sqleq-fuzz filled every table with exactly --rows rows, so a witness that needs an empty table was never drawn
-- witness: t = {(1)}, s = {}: A yields no rows (sum over no rows is NULL), B yields (1)
-- The sqleq-solver axis proves this pair, which is issue #60 (a scalar subquery over no rows);
-- that line stays marked until the solver is fixed.
create table "t" ("id" INTEGER, unique ("id"));
create table "s" ("y" INTEGER NOT NULL);
SELECT 1 AS "one" FROM "t" WHERE (SELECT sum("y") FROM "s") IS NOT NULL;
SELECT 1 AS "one" FROM "t";
