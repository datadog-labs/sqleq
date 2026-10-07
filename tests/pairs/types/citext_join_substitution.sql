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
-- origin: issue #58: why citext is refused rather than made opaque: an opaque type's = is the prover's equality, which
--   substitutes u.c for t.c under the join, and two citext values can be equal while their text differs
-- witness: t = {(1, 'a')}, u = {(1, 'A')} (with the citext extension): A yields ('a'), B yields ('A')
create table "t" ("id" INTEGER, "c" citext, unique ("id"));
create table "u" ("id" INTEGER, "c" citext, unique ("id"));
SELECT "t"."c"::text FROM "t" JOIN "u" ON "t"."c" = "u"."c";
SELECT "u"."c"::text FROM "t" JOIN "u" ON "t"."c" = "u"."c";
