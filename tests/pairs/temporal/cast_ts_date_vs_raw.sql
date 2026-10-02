-- Unless explicitly stated otherwise all files in this repository are licensed under the
-- Apache License Version 2.0.
-- This product includes software developed at Datadog (https://www.datadoghq.com/).
-- Copyright 2026-Present Datadog, Inc.

-- truth: not-equivalent
-- expect frontend: emit
-- expect fuzz: no-counterexample
-- expect qed: no-proof
-- expect sqlsolver-rust: no-proof
-- expect sqlsolver-jvm: no-proof
-- origin: a timestamp-to-date cast was dropped when dates and timestamps shared one IR type (742935f)
-- witness: t = {(1, '2024-01-01 10:00', '2024-01-01')}: A keeps the row, B drops it
create table "t" ("id" INTEGER, "ts" TIMESTAMP, "d" DATE, unique ("id"));
SELECT "id" FROM "t" WHERE CAST("ts" AS DATE) = "d";
SELECT "id" FROM "t" WHERE "ts" = "d";
