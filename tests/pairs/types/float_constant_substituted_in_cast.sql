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
-- origin: issue #84: from x = 0 the provers put the constant 0 for a double precision x inside a cast to text, though -0 = 0
-- witness: t = {('-0')}: A yields '-0', B yields '0'
create table "t" ("x" DOUBLE PRECISION);
SELECT CAST("x" AS TEXT) FROM "t" WHERE "x" = 0;
SELECT CAST(0 AS TEXT) FROM "t" WHERE "x" = 0;
