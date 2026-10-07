-- Unless explicitly stated otherwise all files in this repository are licensed under the
-- Apache License Version 2.0.
-- This product includes software developed at Datadog (https://www.datadoghq.com/).
-- Copyright 2026-Present Datadog, Inc.

-- truth: not-equivalent
-- expect frontend: emit
-- expect fuzz: counterexample
-- expect qed: no-proof
-- expect sqleq-solver: unsupported
-- expect lean: unsupported
-- origin: issue #65: under a literal LIMIT sqleq-fuzz compared only row counts, even when ORDER BY is a total order
-- witness: t = {(1, 0), (2, 1)}, u = {(1, 1), (2, 0)}: A yields (1, 2), B yields (2, 1)
create table "t" ("id" INTEGER PRIMARY KEY, "x" INTEGER NOT NULL UNIQUE);
create table "u" ("id" INTEGER PRIMARY KEY, "y" INTEGER NOT NULL UNIQUE);
SELECT "t"."id" AS "tid", "u"."id" AS "uid" FROM "t" JOIN "u" ON "t"."x" = "u"."y" ORDER BY "t"."id" LIMIT 1;
SELECT "t"."id" AS "tid", "u"."id" AS "uid" FROM "t" JOIN "u" ON "t"."x" = "u"."y" ORDER BY "u"."id" LIMIT 1;
