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
-- origin: issue #108: box = compares areas within a tolerance, so it is not transitive, and it was read as an equivalence
-- witness: t = {(1, '((0,0),(1,1))', '((0,0),(1,1.0000009))', '((0,0),(1,1.0000018))')}: a = b and b = c hold, a = c does not; A yields 1, B yields no row
create table "t" ("id" INTEGER, "a" box, "b" box, "c" box);
SELECT "id" FROM "t" WHERE "a" = "b" AND "b" = "c";
SELECT "id" FROM "t" WHERE "a" = "b" AND "b" = "c" AND "a" = "c";
