-- Unless explicitly stated otherwise all files in this repository are licensed under the
-- Apache License Version 2.0.
-- This product includes software developed at Datadog (https://www.datadoghq.com/).
-- Copyright 2026-Present Datadog, Inc.

-- truth: equivalent
-- expect frontend: refuse:unsupported
-- expect fuzz: no-counterexample
-- expect qed: no-plan
-- expect sqleq-solver: no-plan
-- expect sqlsolver-jvm: no-plan
-- expect lean: unsupported
-- origin: DuckDB binds ->> looser than AND, so sqleq-fuzz once ran a different predicate than Postgres
-- argument: in Postgres ->> binds tighter than IS NULL and AND, so both predicates are `false AND ...`, false for every row
-- Refused since issue #84, which costs QED its proof here. ->> over a jsonb column can tell apart two
-- documents that jsonb's = calls equal, and the two queries are not one plan.
create table "t" ("id" INTEGER, "j" JSONB, unique ("id"));
SELECT "id" FROM "t" WHERE 1 = 2 AND "j" ->> 'k' IS NULL;
SELECT "id" FROM "t" WHERE 1 = 2;
