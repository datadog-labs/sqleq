-- Unless explicitly stated otherwise all files in this repository are licensed under the
-- Apache License Version 2.0.
-- This product includes software developed at Datadog (https://www.datadoghq.com/).
-- Copyright 2026-Present Datadog, Inc.

-- truth: not-equivalent
-- expect frontend: emit
-- expect fuzz: counterexample
-- expect qed: no-proof
-- expect sqleq-solver: unsupported
-- expect lean: unsupported
-- origin: issue #65: sqleq-fuzz bound a pure LIMIT parameter so that it never cut, so a LIMIT on one side only was never seen
-- witness: t = {(1, 0)}, $1 = 0: A yields (1, 0), B yields no rows
-- catalog: inferred-seeded
create table "t" ("id" INTEGER PRIMARY KEY, "a" INTEGER);
SELECT "id", "a" FROM "t";
SELECT "id", "a" FROM "t" ORDER BY "id" LIMIT $1;
