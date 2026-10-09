-- Unless explicitly stated otherwise all files in this repository are licensed under the
-- Apache License Version 2.0.
-- This product includes software developed at Datadog (https://www.datadoghq.com/).
-- Copyright 2026-Present Datadog, Inc.

-- truth: not-equivalent
-- expect frontend: emit
-- expect fuzz: nondet-skip
-- expect qed: no-proof
-- expect sqleq-solver: unsupported
-- expect lean: unsupported
-- origin: issue #46: the qualified ORDER BY key mismatch, inside IN (SELECT ... LIMIT)
-- witness: t = {(1, 1), (2, 2)}, u = {(1, 5), (2, 3)}, w = {(1, 5), (2, 3)}: A returns 1, B returns 2
-- sqleq-fuzz skips the pair since issue #124. Its LIMIT can leave ties, and the IN above it reads the row
-- the LIMIT keeps, so a difference it finds may be one in which tied row each side kept, which says nothing
-- about equivalence. The witness has no ties.
create table "t" ("k" INTEGER, "a" INTEGER);
create table "u" ("k" INTEGER, "a" INTEGER);
create table "w" ("id" INTEGER, "x" INTEGER);
SELECT "w"."id" FROM "w" WHERE "w"."x" IN (SELECT "u"."a" FROM "t" JOIN "u" ON "t"."k" = "u"."k" ORDER BY "t"."a" LIMIT 1);
SELECT "w"."id" FROM "w" WHERE "w"."x" IN (SELECT "u"."a" FROM "t" JOIN "u" ON "t"."k" = "u"."k" ORDER BY "u"."a" LIMIT 1);
