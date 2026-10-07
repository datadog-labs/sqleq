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
-- origin: issue #84: ->> turns a jsonb number into text, scale included, though jsonb = compares numbers by value
-- witness: t = {('{"a": 1.0}')}, u = {('{"a": 1.00}')}: A yields '1.0', B yields '1.00'
create table "t" ("j" JSONB);
create table "u" ("j" JSONB);
SELECT "t"."j" ->> 'a' FROM "t" JOIN "u" ON "t"."j" = "u"."j";
SELECT "u"."j" ->> 'a' FROM "t" JOIN "u" ON "t"."j" = "u"."j";
