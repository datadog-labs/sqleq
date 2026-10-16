-- Unless explicitly stated otherwise all files in this repository are licensed under the
-- Apache License Version 2.0.
-- This product includes software developed at Datadog (https://www.datadoghq.com/).
-- Copyright 2026-Present Datadog, Inc.

-- truth: equivalent
-- expect frontend: emit-reflexive
-- expect fuzz: no-counterexample
-- expect qed: proved-literal
-- expect sqleq-solver: proved-literal
-- expect lean: unsupported
-- origin: issue #177: a UNION ALL column took its first branch's type, so the integer branch reached
--   the prover under a numeric column and QED panicked
-- argument: Postgres resolves the column to numeric and converts i, which is the cast B writes out
create table "t" ("a" NUMERIC, "i" INTEGER);
SELECT "a" FROM "t" UNION ALL SELECT "i" FROM "t";
SELECT "a" FROM "t" UNION ALL SELECT "i"::numeric FROM "t";
