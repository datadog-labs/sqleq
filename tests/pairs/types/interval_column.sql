-- Unless explicitly stated otherwise all files in this repository are licensed under the
-- Apache License Version 2.0.
-- This product includes software developed at Datadog (https://www.datadoghq.com/).
-- Copyright 2026-Present Datadog, Inc.

-- truth: equivalent
-- expect frontend: emit
-- expect fuzz: not-comparable
-- expect qed: no-proof
-- expect sqleq-solver: no-proof
-- expect lean: unsupported
-- origin: issue #89: sqleq-fuzz materialized an interval column as INTEGER (its type name contains INT),
--   so under integer_division x / 2 truncated
-- argument: for a factor of 2 Postgres computes every field of x / 2 and of x * 0.5 with the same double
--   arithmetic (dividing by 2 and multiplying by 0.5 are exact and equal), so the two agree on every
--   interval; Postgres 17 returns the same rows for both on '1 day', '1 mon', '1 mon 1 day 1 second',
--   '-3 days', '1 microsecond', '3 microseconds', '13 mons 5 days 03:00:01.000003' and NULL

create table "t" ("id" INTEGER, "x" INTERVAL, unique ("id"));
SELECT "id", "x" / 2 AS "h" FROM "t";
SELECT "id", "x" * 0.5 AS "h" FROM "t";
