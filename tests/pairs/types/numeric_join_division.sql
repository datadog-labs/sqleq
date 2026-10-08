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
-- origin: issue #84: numeric division rounds to a scale that follows its operands' scales
-- witness: t = {(1.0)}, u = {(1.000000000000000000000)}: A yields 0.33333333333333333333, B yields 0.333333333333333333333
create table "t" ("x" NUMERIC);
create table "u" ("x" NUMERIC);
SELECT "t"."x" / 3 FROM "t" JOIN "u" ON "t"."x" = "u"."x";
SELECT "u"."x" / 3 FROM "t" JOIN "u" ON "t"."x" = "u"."x";
