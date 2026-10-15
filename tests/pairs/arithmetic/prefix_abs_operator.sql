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
-- origin: a prefix operator the frontend does not model is refused (`unary`); read as unary + is,
--   Postgres's absolute value `@ (a - 1)` would lower to `a - 1`. No test reached that refusal until
--   this pair
-- witness: t = {(1, 0)}: A returns 1, B returns -1
create table "t" ("id" INTEGER, "a" INTEGER, unique ("id"));
SELECT @ ("a" - 1) FROM "t";
SELECT "a" - 1 FROM "t";
