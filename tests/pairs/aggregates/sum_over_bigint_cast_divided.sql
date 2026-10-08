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
-- origin: issue #121: a sum over a cast to bigint is a numeric, a sum over the integer a bigint, so one division is numeric and the other integer
-- witness: t = {(1, 1)}: A yields 0.50000000000000000000, B yields 0
create table "t" ("id" INTEGER, "a" INTEGER);
SELECT sum(CAST("a" AS BIGINT)) / 2 FROM "t";
SELECT sum("a") / 2 FROM "t";
