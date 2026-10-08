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
-- origin: a set operation over a DATE and a TIMESTAMP column is refused, not lowered
--   (docs/SOUNDNESS.md); a pin for each documented refusal (#68)
-- witness: t = {(1, '2024-01-01', '2024-01-01 12:00:00')}: A converts d to a timestamp and returns
--   two rows, B converts ts to a date and returns one
create table "t" ("id" INTEGER, "d" DATE, "ts" TIMESTAMP);
SELECT d FROM t UNION SELECT ts FROM t;
SELECT d FROM t UNION SELECT CAST(ts AS DATE) FROM t;
