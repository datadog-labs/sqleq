-- Unless explicitly stated otherwise all files in this repository are licensed under the
-- Apache License Version 2.0.
-- This product includes software developed at Datadog (https://www.datadoghq.com/).
-- Copyright 2026-Present Datadog, Inc.

-- truth: not-equivalent
-- expect frontend: emit
-- expect fuzz: counterexample
-- expect qed: no-proof
-- expect sqleq-solver: no-proof
-- expect lean: unsupported
-- origin: a pin for a documented refusal (#68), lifted by issue #92 item 4: a TIMESTAMP IN a subquery
--   over a DATE column now converts the column, as Postgres does (ts = d::timestamp), and this pair
--   guards the direction: B, which converts ts to a date instead, is another query
-- witness: t = {(1, '2024-01-01', '2024-01-01 12:00:00')}: A compares ts with midnight and returns
--   no rows, B compares dates and returns 1
create table "t" ("id" INTEGER, "d" DATE, "ts" TIMESTAMP);
SELECT id FROM t WHERE ts IN (SELECT d FROM t);
SELECT id FROM t WHERE CAST(ts AS DATE) IN (SELECT d FROM t);
