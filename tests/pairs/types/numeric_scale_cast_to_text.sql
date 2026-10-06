-- Unless explicitly stated otherwise all files in this repository are licensed under the
-- Apache License Version 2.0.
-- This product includes software developed at Datadog (https://www.datadoghq.com/).
-- Copyright 2026-Present Datadog, Inc.

-- truth: not-equivalent
-- expect frontend: refuse:unsupported
-- expect fuzz: no-counterexample
-- expect qed: no-plan
-- expect sqleq-solver: no-plan
-- expect lean: unsupported
-- origin: issue #58: a numeric cast to text was a function of its value, which REAL carries without the scale the text shows
-- witness: t = {(2)}: A yields '2.0', B yields '2.00'
create table "t" ("x" NUMERIC);
SELECT CAST("x" * 1.0 AS TEXT) FROM "t";
SELECT CAST("x" * 1.00 AS TEXT) FROM "t";
