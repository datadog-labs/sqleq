-- Unless explicitly stated otherwise all files in this repository are licensed under the
-- Apache License Version 2.0.
-- This product includes software developed at Datadog (https://www.datadoghq.com/).
-- Copyright 2026-Present Datadog, Inc.

-- truth: not-equivalent
-- expect frontend: emit
-- expect fuzz: counterexample
-- expect qed: no-proof
-- expect sqleq-solver: unsupported
-- expect sqlsolver-jvm: unsupported
-- expect cvc5: no-proof
-- expect lean: unsupported
-- catalog: inferred-seeded
-- origin: a parameter dead on one side counts where the other side uses it, so the pair aligns and
--   is compared, rather than dropped from one side only
-- witness: t = {(1, 2)}, s = {(1, 1)}, $1 = 5, $2 = 2: A returns 1; B also needs a = 5 and returns
--   nothing
create table "t" ("id" INTEGER PRIMARY KEY, "a" INTEGER);
create table "s" ("id" INTEGER PRIMARY KEY, "t_id" INTEGER);
SELECT "id" FROM "t" WHERE EXISTS (SELECT $1 FROM "s" WHERE "s"."t_id" = "t"."id") AND "a" = $2;
SELECT "id" FROM "t" WHERE EXISTS (SELECT 1 FROM "s" WHERE "s"."t_id" = "t"."id") AND "a" = $2 AND "a" = $1;
