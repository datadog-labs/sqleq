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
-- origin: issue #46: the qualified ORDER BY key mismatch, in a grouped query
-- witness: t = {(1, 1), (2, 2)}, u = {(1, 5), (2, 3)}: A returns 5, B returns 3
create table "t" ("k" INTEGER, "a" INTEGER);
create table "u" ("k" INTEGER, "a" INTEGER);
SELECT "u"."a" FROM "t" JOIN "u" ON "t"."k" = "u"."k" GROUP BY "t"."a", "u"."a" ORDER BY "t"."a" LIMIT 1;
SELECT "u"."a" FROM "t" JOIN "u" ON "t"."k" = "u"."k" GROUP BY "t"."a", "u"."a" ORDER BY "u"."a" LIMIT 1;
