-- Unless explicitly stated otherwise all files in this repository are licensed under the
-- Apache License Version 2.0.
-- This product includes software developed at Datadog (https://www.datadoghq.com/).
-- Copyright 2026-Present Datadog, Inc.

-- truth: not-equivalent
-- expect frontend: emit
-- expect fuzz: not-comparable
-- expect qed: no-proof
-- expect sqleq-solver: no-proof
-- expect lean: unsupported
-- origin: issue #61: every / was one untyped divide in sqleq-solver, and its set solver made 2 and 2.0 one constant
-- witness: t = {(1, 2), (2, 3)}: 3 / 2.0 = 1.5 > 1 but 3 / 2 = 1, so A returns 2 and B returns nothing
create table "t" ("id" INTEGER, "a" INTEGER);
SELECT DISTINCT "id" FROM "t" WHERE "a" / 2.0 > 1;
SELECT DISTINCT "id" FROM "t" WHERE "a" / 2 > 1;
