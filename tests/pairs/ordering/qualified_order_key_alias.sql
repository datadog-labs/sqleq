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
-- origin: issue #46: a qualified ORDER BY key is matched against an output alias of the same
--   name, so ORDER BY t.a sorts by the column aliased a (t.b)
-- witness: t = {(1, 2), (2, 1)}: A returns 2, B returns 1
create table "t" ("a" INTEGER, "b" INTEGER);
SELECT "b" AS "a" FROM "t" ORDER BY "t"."a" LIMIT 1;
SELECT "b" AS "a" FROM "t" ORDER BY "b" LIMIT 1;
