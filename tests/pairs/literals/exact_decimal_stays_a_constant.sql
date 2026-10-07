-- Unless explicitly stated otherwise all files in this repository are licensed under the
-- Apache License Version 2.0.
-- This product includes software developed at Datadog (https://www.datadoghq.com/).
-- Copyright 2026-Present Datadog, Inc.

-- truth: equivalent
-- expect frontend: emit
-- expect fuzz: no-counterexample
-- expect qed: proved
-- expect sqleq-solver: no-proof
-- expect lean: unsupported
-- origin: issue #56 (control): a decimal that is exactly an f32 with a numerator and denominator
--   that fit an i32 stays a constant, here at the smallest such power of two, so the QED prover
--   still computes with it
-- argument: x = 2^-30 exactly when x * 2^30 = 1, and numeric arithmetic is exact
create table "t" ("x" NUMERIC);
SELECT "x" FROM "t" WHERE "x" = 0.000000000931322574615478515625;
SELECT "x" FROM "t" WHERE "x" * 1073741824 = 1;
