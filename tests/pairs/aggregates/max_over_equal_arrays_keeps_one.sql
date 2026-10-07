-- Unless explicitly stated otherwise all files in this repository are licensed under the
-- Apache License Version 2.0.
-- This product includes software developed at Datadog (https://www.datadoghq.com/).
-- Copyright 2026-Present Datadog, Inc.

-- truth: not-equivalent
-- expect frontend: emit
-- expect fuzz: no-counterexample
-- expect qed: no-proof
-- expect sqleq-solver: unsupported
-- expect lean: unsupported
-- origin: issue #109: sqleq-solver bound MAX to an input by identity, so two numrange[] arrays that = calls equal and that print differently were two maxima
-- witness: t = {(1, '{"[1.0,2.0)"}'), (2, '{"[1.00,2.00)"}')}: the two arrays are equal, so MAX returns one of them and A yields one row; neither is greater than the other, so B yields {"[1.0,2.0)"} and {"[1.00,2.00)"}
-- The cast to text reaches the provers because the frontend reads = on a type it does not know, numrange[]
-- here, as identity (issue #108). The same plans over numeric (REAL) or interval were proved too.
create table "t" ("id" INTEGER NOT NULL, "x" numrange[] NOT NULL);
SELECT DISTINCT CAST("m" AS TEXT) FROM (SELECT MAX("x") AS "m" FROM "t") AS "s" WHERE "m" IS NOT NULL;
SELECT DISTINCT CAST("t"."x" AS TEXT) FROM "t" LEFT JOIN "t" AS "u" ON "u"."x" > "t"."x" WHERE "u"."id" IS NULL;
