-- Unless explicitly stated otherwise all files in this repository are licensed under the
-- Apache License Version 2.0.
-- This product includes software developed at Datadog (https://www.datadoghq.com/).
-- Copyright 2026-Present Datadog, Inc.

-- truth: not-equivalent
-- expect frontend: emit
-- expect fuzz: counterexample
-- expect qed: no-proof
-- expect sqleq-solver: no-proof
-- expect lean: unsupported
-- origin: issue #61: from a = 2.0, sqleq-solver substituted the decimal constant for the integer column inside a cast
-- witness: t = {(1, 2)}: a = 2.0 holds, but a is the integer 2, so A yields '2' and B yields '2.0'
-- Lowered since issue #84, which reads a numeric literal under a cast to text through q_exact_real, a
-- function of the literal's spelling that no prover reads as a number.
create table "t" ("id" INTEGER, "a" INTEGER);
SELECT CAST("a" AS TEXT) FROM "t" WHERE "a" = 2.0;
SELECT CAST(2.0 AS TEXT) FROM "t" WHERE "a" = 2.0;
