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
-- origin: issue #64: sqleq-fuzz compared STRUCT cells by their Debug rendering, which includes the DuckDB type of each field
-- argument: b is already bigint, so the cast changes nothing
create table "t" ("id" INTEGER, "b" BIGINT, unique ("id"));
SELECT "id", ROW("b") AS "r" FROM "t";
SELECT "id", ROW("b"::bigint) AS "r" FROM "t";
