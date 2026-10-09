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
-- origin: a cast over a parameter was dropped as its type whatever type inference gave it; $1 is a numeric here (its first use is n = $1) and $1::int rounds it
-- witness: t = {(1, 0.4)}, $1 = 0.4: A keeps the row (0.4::int = 0) and B drops it (0.4 = 0 is false)
create table "t" ("id" INTEGER, "n" NUMERIC);
SELECT "id" FROM "t" WHERE "n" = $1 AND $1::int = 0;
SELECT "id" FROM "t" WHERE "n" = $1 AND $1 = 0;
