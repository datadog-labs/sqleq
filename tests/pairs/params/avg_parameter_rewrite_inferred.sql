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
-- catalog: inferred
-- origin: issue #122: the avg rewrite under the inferring catalog, whose own schema types a as an integer too
-- witness: t = {(1, 0), (1, 1)}, $1 = 0.5: the mean is 0.5, so A returns 1 and B returns no row
create table "t" ("id" INTEGER, "a" INTEGER);
SELECT "id" FROM "t" GROUP BY "id" HAVING avg("a") = $1 AND $1 < 1;
SELECT "id" FROM "t" GROUP BY "id" HAVING avg("a") = $1 AND $1 <= 0;
