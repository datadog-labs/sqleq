-- Unless explicitly stated otherwise all files in this repository are licensed under the
-- Apache License Version 2.0.
-- This product includes software developed at Datadog (https://www.datadoghq.com/).
-- Copyright 2026-Present Datadog, Inc.

-- truth: not-equivalent
-- expect frontend: emit
-- expect fuzz: counterexample
-- expect qed: no-proof
-- expect sqleq-solver: no-proof
-- expect sqlsolver-jvm: no-proof
-- expect lean: unsupported
-- origin: dates and timestamps both lowered as integers, so `d + 1` read as one microsecond (742935f)
-- witness: t = {(1, '2024-01-01 10:00', '2024-01-01')}: A keeps the row, B drops it
create table "t" ("id" INTEGER, "ts" TIMESTAMP, "d" DATE, unique ("id"));
SELECT "id" FROM "t" WHERE "ts" < "d" + 1;
SELECT "id" FROM "t" WHERE "ts" <= "d";
