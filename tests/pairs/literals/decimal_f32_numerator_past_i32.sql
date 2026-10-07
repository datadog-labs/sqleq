-- Unless explicitly stated otherwise all files in this repository are licensed under the
-- Apache License Version 2.0.
-- This product includes software developed at Datadog (https://www.datadoghq.com/).
-- Copyright 2026-Present Datadog, Inc.

-- truth: not-equivalent
-- expect frontend: emit
-- expect fuzz: no-counterexample
-- expect qed: no-proof
-- expect sqleq-solver: no-proof
-- expect lean: unsupported
-- origin: issue #56: the QED prover panicked on a decimal constant whose f32 numerator does not fit
--   an i32
-- witness: t = {(3000000000.5)}: A yields the row, B yields none
create table "t" ("x" NUMERIC);
SELECT "x" FROM "t" WHERE "x" = 3000000000.5;
SELECT "x" FROM "t" WHERE "x" = 3000000001.5;
