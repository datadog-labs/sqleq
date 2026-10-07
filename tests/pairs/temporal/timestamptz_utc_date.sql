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
-- origin: issue #62: sqleq-fuzz materialized timestamptz as a naive TIMESTAMP in the host's time zone, so the verdict depended on the machine's TZ
-- argument: for a timestamptz, (ts AT TIME ZONE 'UTC')::date is its UTC calendar date, which is 2020-01-02 exactly when ts lies in [2020-01-02 00:00+00, 2020-01-03 00:00+00), in every session time zone
create table "t" ("id" INTEGER, "ts" TIMESTAMPTZ, unique ("id"));
SELECT "id" FROM "t" WHERE ("ts" AT TIME ZONE 'UTC')::date = DATE '2020-01-02';
SELECT "id" FROM "t" WHERE "ts" >= TIMESTAMPTZ '2020-01-02 00:00:00+00' AND "ts" < TIMESTAMPTZ '2020-01-03 00:00:00+00';
