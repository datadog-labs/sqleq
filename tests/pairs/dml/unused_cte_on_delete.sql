-- Unless explicitly stated otherwise all files in this repository are licensed under the
-- Apache License Version 2.0.
-- This product includes software developed at Datadog (https://www.datadoghq.com/).
-- Copyright 2026-Present Datadog, Inc.

-- truth: equivalent
-- expect frontend: emit-reflexive
-- expect fuzz: no-counterexample
-- expect qed: proved-literal
-- expect sqlsolver-rust: proved-literal
-- expect sqlsolver-jvm: proved-literal
-- origin: sqleq-fuzz measured a WITH-wrapped DELETE as a query rather than by the table it changes
-- argument: the CTE is never referenced and modifies nothing, so it has no effect
create table "t" ("id" INTEGER, "a" INTEGER, unique ("id"));
WITH "unused" AS (SELECT 1) DELETE FROM "t" WHERE "a" = 1;
DELETE FROM "t" WHERE "a" = 1;
