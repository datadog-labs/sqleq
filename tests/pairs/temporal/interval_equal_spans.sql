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
-- origin: issue #89: sqleq-fuzz compared interval cells by their fields, so '1 day' and '24 hours' differed
-- argument: INTERVAL '1 day' = INTERVAL '24 hours' in Postgres (interval_cmp compares the whole span, a day
--   as 24 hours), so both sides return one value per row that are equal under =

create table "t" ("id" INTEGER, unique ("id"));
SELECT INTERVAL '1 day' FROM "t";
SELECT INTERVAL '24 hours' FROM "t";
