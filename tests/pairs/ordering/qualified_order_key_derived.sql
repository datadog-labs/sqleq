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
-- origin: issue #46: the qualified ORDER BY key mismatch, inside a derived table
-- witness: t = {(1, 1), (2, 2)}, u = {(1, 5), (2, 3)}: A returns 5, B returns 3
create table "t" ("k" INTEGER, "a" INTEGER);
create table "u" ("k" INTEGER, "a" INTEGER);
SELECT "s"."a" FROM (SELECT "u"."a" FROM "t" JOIN "u" ON "t"."k" = "u"."k" ORDER BY "t"."a" LIMIT 1) AS "s";
SELECT "s"."a" FROM (SELECT "u"."a" FROM "t" JOIN "u" ON "t"."k" = "u"."k" ORDER BY "u"."a" LIMIT 1) AS "s";
