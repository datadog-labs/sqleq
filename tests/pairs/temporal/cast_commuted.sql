-- Unless explicitly stated otherwise all files in this repository are licensed under the
-- Apache License Version 2.0.
-- This product includes software developed at Datadog (https://www.datadoghq.com/).
-- Copyright 2026-Present Datadog, Inc.

-- truth: equivalent
-- expect frontend: emit
-- expect fuzz: no-counterexample
-- expect qed: proved
-- expect sqlsolver-rust: proved
-- expect sqlsolver-jvm: proved
-- expect lean: unsupported
-- origin: the equivalent shape beside the two temporal casts above, which must still lower and prove
-- argument: = is symmetric, and both sides apply the same cast to the same column
create table "t" ("id" INTEGER, "ts" TIMESTAMP, "d" DATE, unique ("id"));
SELECT "id" FROM "t" WHERE CAST("ts" AS DATE) = "d";
SELECT "id" FROM "t" WHERE "d" = CAST("ts" AS DATE);
