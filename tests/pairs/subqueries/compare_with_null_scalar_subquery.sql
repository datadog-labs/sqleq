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
-- origin: issue #60: a comparison with a NULL-valued scalar subquery was read as FALSE, not UNKNOWN, so NOT made it TRUE
-- witness: t = {(1, 5)}, s = {}: 5 = NULL is UNKNOWN, so A returns nothing, and UNKNOWN IS NOT TRUE holds, so B returns 1
create table "t" ("id" INTEGER, "x" INTEGER NOT NULL);
create table "s" ("k" INTEGER, "y" INTEGER);
SELECT "id" FROM "t" WHERE NOT ("x" = (SELECT sum("y") FROM "s"));
SELECT "id" FROM "t" WHERE ("x" = (SELECT sum("y") FROM "s")) IS NOT TRUE;
