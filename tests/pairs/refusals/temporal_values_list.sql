-- Unless explicitly stated otherwise all files in this repository are licensed under the
-- Apache License Version 2.0.
-- This product includes software developed at Datadog (https://www.datadoghq.com/).
-- Copyright 2026-Present Datadog, Inc.

-- truth: not-equivalent
-- expect frontend: refuse:unsupported
-- expect fuzz: no-counterexample
-- expect qed: no-plan
-- expect sqleq-solver: no-plan
-- expect lean: unsupported
-- origin: a VALUES list that holds a DATE and a TIMESTAMP in one column is refused, not lowered
--   (docs/SOUNDNESS.md); a pin for each documented refusal (#68)
-- witness: t = {(1, '2024-01-01', '2024-01-01 06:00:00')}: A compares ts with midnight and noon and
--   returns no rows, B compares dates and returns 1
create table "t" ("id" INTEGER, "d" DATE, "ts" TIMESTAMP);
SELECT id FROM t WHERE ts IN (VALUES (DATE '2024-01-01'), (TIMESTAMP '2024-01-01 12:00:00'));
SELECT id FROM t WHERE CAST(ts AS DATE) IN (VALUES (DATE '2024-01-01'));
