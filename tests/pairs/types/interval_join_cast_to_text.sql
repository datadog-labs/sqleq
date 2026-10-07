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
-- origin: issue #84: '1 day' = '24 hours' as intervals, but their text differs
-- witness: t = {('1 day')}, u = {('24 hours')}: A yields '1 day', B yields '24:00:00'
create table "t" ("x" INTERVAL);
create table "u" ("x" INTERVAL);
SELECT CAST("t"."x" AS TEXT) FROM "t" JOIN "u" ON "t"."x" = "u"."x";
SELECT CAST("u"."x" AS TEXT) FROM "t" JOIN "u" ON "t"."x" = "u"."x";
