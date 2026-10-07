-- Unless explicitly stated otherwise all files in this repository are licensed under the
-- Apache License Version 2.0.
-- This product includes software developed at Datadog (https://www.datadoghq.com/).
-- Copyright 2026-Present Datadog, Inc.

-- truth: not-equivalent
-- expect frontend: emit
-- expect fuzz: counterexample
-- expect qed: proved !known-unsound
-- expect sqleq-solver: no-proof
-- expect lean: unsupported
-- origin: issue #61: sqleq-solver compared decimal constants through f64, and 0.1 and 0.10000000000000001 are one double
-- witness: t = {(1, 2)}: numeric is exact and 0.10000000000000001 > 0.1, so A returns 1 and B returns nothing
-- The qed pin is the QED prover reading decimal literals through f32 (issue #56), not sqleq-solver.
create table "t" ("id" INTEGER, "a" NUMERIC);
SELECT "id" FROM "t" WHERE 0.10000000000000001 > 0.1;
SELECT "id" FROM "t" WHERE FALSE;
