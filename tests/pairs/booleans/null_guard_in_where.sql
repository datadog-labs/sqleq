-- Unless explicitly stated otherwise all files in this repository are licensed under the
-- Apache License Version 2.0.
-- This product includes software developed at Datadog (https://www.datadoghq.com/).
-- Copyright 2026-Present Datadog, Inc.

-- truth: equivalent
-- expect frontend: emit
-- expect fuzz: no-counterexample
-- expect qed: proved
-- expect sqlsolver-rust: proved
-- expect sqlsolver-jvm: proved
-- expect lean: unsupported
-- catalog: inferred-seeded
-- origin: the control beside booleans/projected_null_guard.sql
-- argument: `x <= 5` is NULL exactly when x is NULL, and WHERE drops a row on NULL as on false
create table "t" ("id" INTEGER, "x" INTEGER, unique ("id"));
SELECT "id" FROM "t" WHERE "x" IS NOT NULL AND "x" <= 5 AND "id" = $1;
SELECT "id" FROM "t" WHERE "x" <= 5 AND "id" = $1;
