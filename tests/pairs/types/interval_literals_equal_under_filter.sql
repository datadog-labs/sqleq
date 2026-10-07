-- Unless explicitly stated otherwise all files in this repository are licensed under the
-- Apache License Version 2.0.
-- This product includes software developed at Datadog (https://www.datadoghq.com/).
-- Copyright 2026-Present Datadog, Inc.

-- truth: not-equivalent
-- expect frontend: emit
-- expect fuzz: not-comparable
-- expect qed: no-proof
-- expect sqleq-solver: no-proof
-- expect lean: unsupported
-- origin: issue #84: a filter that makes two interval literals equal does not make a date plus one the date plus the other
-- witness: t = {('2024-01-31', '1 mon')}: A yields 2024-02-29 00:00:00, B yields 2024-03-01 00:00:00
create table "t" ("d" DATE, "x" INTERVAL);
SELECT "d" + INTERVAL '1 mon' FROM "t" WHERE "x" = INTERVAL '1 mon' AND "x" = INTERVAL '30 days';
SELECT "d" + INTERVAL '30 days' FROM "t" WHERE "x" = INTERVAL '1 mon' AND "x" = INTERVAL '30 days';
