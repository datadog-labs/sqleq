-- Unless explicitly stated otherwise all files in this repository are licensed under the
-- Apache License Version 2.0.
-- This product includes software developed at Datadog (https://www.datadoghq.com/).
-- Copyright 2026-Present Datadog, Inc.

-- truth: not-equivalent
-- expect frontend: refuse:unsupported
-- expect fuzz: counterexample
-- expect qed: no-plan
-- expect sqleq-solver: no-plan
-- expect lean: unsupported
-- origin: issue #52: WITHIN GROUP was never read, so both calls were percentile_cont(0.5)
-- witness: t = {(1, 10)}: A yields 1, B yields 10
create table "t" ("a" INTEGER, "b" INTEGER);
SELECT percentile_cont(0.5) WITHIN GROUP (ORDER BY "a") FROM "t";
SELECT percentile_cont(0.5) WITHIN GROUP (ORDER BY "b") FROM "t";
