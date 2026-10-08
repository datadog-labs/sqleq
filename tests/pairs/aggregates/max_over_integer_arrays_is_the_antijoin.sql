-- Unless explicitly stated otherwise all files in this repository are licensed under the
-- Apache License Version 2.0.
-- This product includes software developed at Datadog (https://www.datadoghq.com/).
-- Copyright 2026-Present Datadog, Inc.

-- truth: equivalent
-- expect frontend: emit
-- expect fuzz: no-counterexample
-- expect qed: no-proof
-- expect sqleq-solver: proved
-- expect lean: unsupported
-- origin: issue #109: the control for MAX over numrange[]; = on integer[] is identity, so MAX binds to an input by identity faithfully and sqleq-solver still proves the pair
-- argument: x is NOT NULL and integer[]'s order is total, with no two distinct arrays that = calls equal; so on an empty t both sides are empty, and otherwise the inputs with nothing greater are exactly the copies of MAX(x), one value with one text
create table "t" ("id" INTEGER NOT NULL, "x" INTEGER[] NOT NULL);
SELECT DISTINCT CAST("m" AS TEXT) FROM (SELECT MAX("x") AS "m" FROM "t") AS "s" WHERE "m" IS NOT NULL;
SELECT DISTINCT CAST("t"."x" AS TEXT) FROM "t" LEFT JOIN "t" AS "u" ON "u"."x" > "t"."x" WHERE "u"."id" IS NULL;
