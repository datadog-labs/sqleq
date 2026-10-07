-- Unless explicitly stated otherwise all files in this repository are licensed under the
-- Apache License Version 2.0.
-- This product includes software developed at Datadog (https://www.datadoghq.com/).
-- Copyright 2026-Present Datadog, Inc.

-- truth: not-equivalent
-- expect frontend: refuse:unsupported
-- expect fuzz: not-comparable
-- expect qed: no-plan
-- expect sqleq-solver: no-plan
-- expect lean: unsupported
-- origin: issue #84: '1 mon' = '30 days' as intervals, but a date plus one is not the date plus the other
-- witness: t = {('2024-01-31', '1 mon')}, u = {('30 days')}: A yields 2024-02-29 00:00:00, B yields 2024-03-01 00:00:00
create table "t" ("d" DATE, "x" INTERVAL);
create table "u" ("x" INTERVAL);
SELECT "t"."d" + "t"."x" FROM "t" JOIN "u" ON "t"."x" = "u"."x";
SELECT "t"."d" + "u"."x" FROM "t" JOIN "u" ON "t"."x" = "u"."x";
