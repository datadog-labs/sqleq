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
-- origin: issue #109: sqleq-solver bound MIN to an input by identity, so two numrange[] arrays that = calls equal and that print differently were two minima
-- witness: t = {(1, '{"[1.0,2.0)"}'), (2, '{"[1.00,2.00)"}')}: MIN returns one of the two equal arrays, so A yields one row; neither is less than the other, so B yields both
create table "t" ("id" INTEGER NOT NULL, "x" numrange[] NOT NULL);
SELECT DISTINCT CAST("m" AS TEXT) FROM (SELECT MIN("x") AS "m" FROM "t") AS "s" WHERE "m" IS NOT NULL;
SELECT DISTINCT CAST("t"."x" AS TEXT) FROM "t" LEFT JOIN "t" AS "u" ON "u"."x" < "t"."x" WHERE "u"."id" IS NULL;
