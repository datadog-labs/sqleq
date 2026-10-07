-- Unless explicitly stated otherwise all files in this repository are licensed under the
-- Apache License Version 2.0.
-- This product includes software developed at Datadog (https://www.datadoghq.com/).
-- Copyright 2026-Present Datadog, Inc.

-- truth: not-equivalent
-- expect frontend: emit
-- expect fuzz: no-counterexample
-- expect qed: proved !known-unsound
-- expect sqleq-solver: no-proof
-- expect lean: unsupported
-- origin: found while fixing issue #61: sqleq-solver read a = b on two numeric columns as identity, but 2.0 = 2.00 holds and the two cast differently
-- witness: t = {(1, 2.0, 2.00)}: a = b holds, A yields '2.0' and B yields '2.00'
-- The qed pin is the QED prover reading numeric equality as identity of the two values, outside this fix.
create table "t" ("id" INTEGER, "a" NUMERIC, "b" NUMERIC);
SELECT CAST("a" AS TEXT) FROM "t" WHERE "a" = "b";
SELECT CAST("b" AS TEXT) FROM "t" WHERE "a" = "b";
