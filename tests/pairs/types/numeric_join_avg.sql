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
-- origin: issue #84: avg over numeric divides, and the scale of the quotient follows the scale of the sum
-- witness: t = {(1.0), (0), (0)}, u = {(1.000000000000000000000), (0)}: A yields 0.33333333333333333333, B yields 0.333333333333333333333
create table "t" ("x" NUMERIC);
create table "u" ("x" NUMERIC);
SELECT AVG("t"."x") FROM "t" JOIN "u" ON "t"."x" = "u"."x";
SELECT AVG("u"."x") FROM "t" JOIN "u" ON "t"."x" = "u"."x";
