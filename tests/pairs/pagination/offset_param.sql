-- Unless explicitly stated otherwise all files in this repository are licensed under the
-- Apache License Version 2.0.
-- This product includes software developed at Datadog (https://www.datadoghq.com/).
-- Copyright 2026-Present Datadog, Inc.

-- truth: not-equivalent
-- expect frontend: emit
-- expect fuzz: counterexample
-- expect qed: no-proof
-- expect sqleq-solver: no-proof
-- expect lean: unsupported
-- origin: issue #65: sqleq-fuzz bound a pure OFFSET parameter to 1000000000, so both sides returned no rows
-- witness: t = {(1, 0)}, $1 = 1, $2 = 0: A yields (1, 0), B yields (1, 1)
-- catalog: inferred-seeded
create table "t" ("id" INTEGER PRIMARY KEY, "a" INTEGER);
SELECT "id", "a" FROM "t" ORDER BY "id" LIMIT $1 OFFSET $2;
SELECT "id", "a" + 1 AS "a" FROM "t" ORDER BY "id" LIMIT $1 OFFSET $2;
