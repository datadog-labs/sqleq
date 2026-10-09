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
-- origin: issue #122: Postgres types an untyped $1 at its first use, here numeric from n + 0; inference typed it an integer from the literals
-- witness: t = {(1, 0.5)}, $1 = 0.5: A returns 1 and B returns no row
create table "t" ("id" INTEGER, "n" NUMERIC);
SELECT "id" FROM "t" WHERE "n" + 0 = $1 AND $1 > 0 AND $1 < 1;
SELECT "id" FROM "t" WHERE false;
