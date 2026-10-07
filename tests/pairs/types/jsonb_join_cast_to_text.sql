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
-- origin: issue #84: jsonb = compares numbers by value, but jsonb text keeps the scale
-- witness: t = {('{"a": 1.0}')}, u = {('{"a": 1.00}')}: A yields '{"a": 1.0}', B yields '{"a": 1.00}'
create table "t" ("x" JSONB);
create table "u" ("x" JSONB);
SELECT CAST("t"."x" AS TEXT) FROM "t" JOIN "u" ON "t"."x" = "u"."x";
SELECT CAST("u"."x" AS TEXT) FROM "t" JOIN "u" ON "t"."x" = "u"."x";
