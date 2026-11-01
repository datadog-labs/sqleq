-- Unless explicitly stated otherwise all files in this repository are licensed under the
-- Apache License Version 2.0.
-- This product includes software developed at Datadog (https://www.datadoghq.com/).
-- Copyright 2026-Present Datadog, Inc.

-- truth: equivalent
-- expect frontend: emit
-- expect fuzz: no-counterexample
-- expect qed: proved
-- expect sqleq-solver: no-proof
-- expect lean: unsupported
-- origin: issue #177: a VALUES column took its first row's type, so the integer row reached the
--   prover under a numeric column and QED panicked
-- argument: Postgres resolves the column to numeric; the second row is 1 on both sides, and 1 = 1.0
--   (they print differently, but rows equal under = are the same rows)
create table "t" ("a" INTEGER);
SELECT "a", "x" FROM "t", (VALUES (1.5), (1)) AS "v"("x");
SELECT "a", "x" FROM "t", (VALUES (1.5), (1.0)) AS "v"("x");
