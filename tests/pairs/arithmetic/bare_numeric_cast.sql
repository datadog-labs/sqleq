-- Unless explicitly stated otherwise all files in this repository are licensed under the
-- Apache License Version 2.0.
-- This product includes software developed at Datadog (https://www.datadoghq.com/).
-- Copyright 2026-Present Datadog, Inc.

-- truth: equivalent
-- expect frontend: emit
-- expect fuzz: no-counterexample
-- expect qed: no-proof
-- expect sqleq-solver: no-proof
-- expect lean: unsupported
-- origin: issue #62: a bare `numeric` cast target reached DuckDB, which reads it as DECIMAL(18,3)
-- argument: a Postgres numeric with no typmod keeps every digit, so a * 0.0001 > 0 exactly when a > 0
create table "t" ("id" INTEGER, "a" INTEGER, unique ("id"));
SELECT "id" FROM "t" WHERE CAST("a" * 0.0001 AS numeric) > 0;
SELECT "id" FROM "t" WHERE "a" > 0;
