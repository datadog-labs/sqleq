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
-- origin: issue #89: sqleq-fuzz let DuckDB answer inf for exp(2000), where Postgres raises
-- argument: exp(0) = 1 > 0; for any other a, exp(a * 1000.0::float8) raises 'value out of range:
--   overflow' (a > 0) or 'underflow' (a < 0) in Postgres, so wherever A runs without an error it selects
--   exactly the rows with a = 0

create table "t" ("id" INTEGER, "a" INTEGER, unique ("id"));
SELECT "id" FROM "t" WHERE exp(CAST("a" AS DOUBLE PRECISION) * 1000) > 0;
SELECT "id" FROM "t" WHERE "a" = 0;
