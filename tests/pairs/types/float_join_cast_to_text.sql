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
-- origin: issue #84: '-0'::float8 = 0, and the provers put one side of t.x = u.x for the other inside a cast to text, where the two differ
-- witness: t = {('-0')}, u = {(0)}: A yields '-0', B yields '0'
create table "t" ("x" DOUBLE PRECISION);
create table "u" ("x" DOUBLE PRECISION);
SELECT CAST("t"."x" AS TEXT) FROM "t" JOIN "u" ON "t"."x" = "u"."x";
SELECT CAST("u"."x" AS TEXT) FROM "t" JOIN "u" ON "t"."x" = "u"."x";
