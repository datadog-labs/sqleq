-- Unless explicitly stated otherwise all files in this repository are licensed under the
-- Apache License Version 2.0.
-- This product includes software developed at Datadog (https://www.datadoghq.com/).
-- Copyright 2026-Present Datadog, Inc.

-- truth: not-equivalent
-- expect frontend: emit
-- expect fuzz: no-counterexample
-- expect qed: no-proof
-- expect sqleq-solver: no-proof
-- expect lean: unsupported
-- origin: issue #87: a string comparison was decided by code point after substituting through a join, but the columns compare under en_US.utf8
-- witness: t = {(1, 'B')}, u = {('B')}: under en_US.utf8 'B' < 'a' is false, so A returns nothing and B returns 1
create table "t" ("id" INTEGER, "s" TEXT COLLATE "en_US.utf8");
create table "u" ("s" TEXT COLLATE "en_US.utf8");
SELECT "t"."id" FROM "t" JOIN "u" ON "t"."s" = "u"."s" WHERE "u"."s" = 'B' AND "t"."s" < 'a';
SELECT "t"."id" FROM "t" JOIN "u" ON "t"."s" = "u"."s" WHERE "u"."s" = 'B';
