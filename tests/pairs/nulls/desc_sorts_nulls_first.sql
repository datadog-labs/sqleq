-- Unless explicitly stated otherwise all files in this repository are licensed under the
-- Apache License Version 2.0.
-- This product includes software developed at Datadog (https://www.datadoghq.com/).
-- Copyright 2026-Present Datadog, Inc.

-- truth: equivalent
-- expect frontend: refuse:unsupported
-- expect fuzz: no-counterexample
-- expect qed: no-plan
-- expect sqleq-solver: no-plan
-- expect lean: unsupported
-- origin: issue #62: sqleq-fuzz sorted NULLs last under DESC (DuckDB's default); Postgres sorts them first
-- argument: in Postgres DESC means DESC NULLS FIRST, so the two window orders are the same order
create table "t" ("id" INTEGER NOT NULL, "a" INTEGER, unique ("id"));
SELECT "id", row_number() OVER (ORDER BY "a" DESC, "id") AS "rn" FROM "t";
SELECT "id", row_number() OVER (ORDER BY "a" DESC NULLS FIRST, "id") AS "rn" FROM "t";
