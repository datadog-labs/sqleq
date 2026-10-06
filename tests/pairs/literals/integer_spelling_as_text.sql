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
-- origin: issue #56: an integer literal reached the QED prover spelled as written, and QED reads a
--   constant cast to text as its spelling, so 007::text was '007' where Postgres prints '7'
-- witness: t = {(1)}: A yields '7', B yields '007'
create table "t" ("a" INTEGER);
SELECT CAST(007 AS TEXT) FROM "t";
SELECT '007' FROM "t";
