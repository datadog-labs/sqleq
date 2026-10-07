-- Unless explicitly stated otherwise all files in this repository are licensed under the
-- Apache License Version 2.0.
-- This product includes software developed at Datadog (https://www.datadoghq.com/).
-- Copyright 2026-Present Datadog, Inc.

-- truth: not-equivalent
-- expect frontend: emit
-- expect fuzz: not-comparable
-- expect qed: proved !known-unsound
-- expect sqleq-solver: no-proof
-- expect lean: unsupported
-- origin: issue #86: sqleq-solver read = on two double precision values as identity, though 0 = -0 holds and the two cast differently
-- witness: t = {(1, 0, -0)}: a = b holds, A yields '0' and B yields '-0'
-- The qed pin is the QED prover reading = as identity on every type, the frontend half of issue #86.
create table "t" ("id" INTEGER, "a" DOUBLE PRECISION, "b" DOUBLE PRECISION);
SELECT CAST("a" AS TEXT) FROM "t" WHERE "a" = "b";
SELECT CAST("b" AS TEXT) FROM "t" WHERE "a" = "b";
