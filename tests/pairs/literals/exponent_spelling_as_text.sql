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
-- origin: issue #56: an exponent literal reached the QED prover spelled as written, and QED reads a
--   constant cast to text as its spelling, so 1e1::text was '1e1' where Postgres prints '10'
-- witness: t = {(1)}: A yields '10', B yields '1e1'
create table "t" ("a" INTEGER);
SELECT CAST(1e1 AS TEXT) FROM "t";
SELECT '1e1' FROM "t";
