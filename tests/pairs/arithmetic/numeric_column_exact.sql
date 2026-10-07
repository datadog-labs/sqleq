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
-- origin: issue #62: sqleq-fuzz materialized NUMERIC columns as DOUBLE, so decimal arithmetic rounded
-- argument: numeric addition is exact, so x + 0.1 + 0.2 = x + 0.3 holds for every non-NULL x and is NULL for a NULL x
create table "t" ("id" INTEGER, "x" NUMERIC(10,2), unique ("id"));
SELECT "id" FROM "t" WHERE "x" + 0.1 + 0.2 = "x" + 0.3;
SELECT "id" FROM "t" WHERE "x" IS NOT NULL;
