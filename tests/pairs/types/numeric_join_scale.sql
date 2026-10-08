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
-- origin: issue #84: scale() reads the scale that numeric equality ignores
-- witness: t = {(2.0)}, u = {(2.00)}: A yields 1, B yields 2
create table "t" ("x" NUMERIC);
create table "u" ("x" NUMERIC);
SELECT scale("t"."x") FROM "t" JOIN "u" ON "t"."x" = "u"."x";
SELECT scale("u"."x") FROM "t" JOIN "u" ON "t"."x" = "u"."x";
