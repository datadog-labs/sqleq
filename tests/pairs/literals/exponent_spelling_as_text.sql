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
-- origin: issue #56: an exponent literal reached the QED prover spelled as written, and QED reads a
--   constant cast to text as its spelling, so 1e1::text was '1e1' where Postgres prints '10'
-- witness: t = {(1)}: A yields '10', B yields '1e1'
-- Lowered since issue #84, which reads a numeric literal under a cast to text through q_exact_real, a
-- function of the literal's spelling that no prover reads as a number.
create table "t" ("a" INTEGER);
SELECT CAST(1e1 AS TEXT) FROM "t";
SELECT '1e1' FROM "t";
