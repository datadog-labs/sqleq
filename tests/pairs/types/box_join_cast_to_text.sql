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
-- origin: issue #108: box = compares areas, and box was read as a type whose = is identity; now refused as a type whose = is not transitive
-- witness: t = {(1, '((0,0),(1,1))', '((0,0),(2,0.5))')}: a = b holds (both areas are 1), A yields (1,1),(0,0) and B yields (2,0.5),(0,0)
create table "t" ("id" INTEGER, "a" box, "b" box);
SELECT CAST("a" AS TEXT) FROM "t" WHERE "a" = "b";
SELECT CAST("b" AS TEXT) FROM "t" WHERE "a" = "b";
