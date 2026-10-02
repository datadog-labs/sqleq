-- Unless explicitly stated otherwise all files in this repository are licensed under the
-- Apache License Version 2.0.
-- This product includes software developed at Datadog (https://www.datadoghq.com/).
-- Copyright 2026-Present Datadog, Inc.

-- truth: equivalent
-- expect frontend: emit
-- expect fuzz: no-counterexample
-- expect qed: proved
-- expect sqlsolver-rust: unsupported
-- expect sqlsolver-jvm: proved
-- origin: DuckDB binds ->> looser than AND, so sqleq-fuzz once ran a different predicate than Postgres
-- argument: in Postgres ->> binds tighter than IS NULL and AND, so both predicates are `false AND ...`, false for every row
create table "t" ("id" INTEGER, "j" JSONB, unique ("id"));
SELECT "id" FROM "t" WHERE 1 = 2 AND "j" ->> 'k' IS NULL;
SELECT "id" FROM "t" WHERE 1 = 2;
