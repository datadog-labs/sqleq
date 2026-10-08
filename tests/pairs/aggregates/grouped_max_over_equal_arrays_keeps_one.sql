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
-- origin: issue #109: sqleq-solver bound a grouped MAX to an input by identity, so two numrange[] arrays of one group that = calls equal were two maxima
-- witness: t = {(1, 1, '{"[1.0,2.0)"}'), (2, 1, '{"[1.00,2.00)"}')}: group 1 has one MAX, so A yields one row; B yields (1, {"[1.0,2.0)"}) and (1, {"[1.00,2.00)"})
create table "t" ("id" INTEGER NOT NULL, "g" INTEGER NOT NULL, "x" numrange[] NOT NULL);
SELECT DISTINCT "g", CAST("m" AS TEXT) FROM (SELECT "g", MAX("x") AS "m" FROM "t" GROUP BY "g") AS "s";
SELECT DISTINCT "t"."g", CAST("t"."x" AS TEXT) FROM "t" LEFT JOIN "t" AS "u" ON "u"."g" = "t"."g" AND "u"."x" > "t"."x" WHERE "u"."id" IS NULL;
