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
-- catalog: inferred-seeded
-- origin: issue #122: the integer-only rewrite against an IN subquery of numerics, which makes $1 a numeric
-- witness: t = {(1, 0.25)}, $1 = 0.5: A returns 1 and B returns no row
create table "t" ("id" INTEGER, "n" NUMERIC);
SELECT "id" FROM "t" WHERE $1 IN (SELECT "n" * 2 FROM "t") AND $1 < 1;
SELECT "id" FROM "t" WHERE $1 IN (SELECT "n" * 2 FROM "t") AND $1 <= 0;
