-- Unless explicitly stated otherwise all files in this repository are licensed under the
-- Apache License Version 2.0.
-- This product includes software developed at Datadog (https://www.datadoghq.com/).
-- Copyright 2026-Present Datadog, Inc.

-- truth: equivalent
-- expect frontend: emit
-- expect fuzz: no-counterexample
-- expect qed: proved
-- expect sqleq-solver: unsupported
-- expect lean: unsupported
-- origin: issue #63: `LIMIT (1)` escaped sqleq-fuzz's literal-LIMIT guard, so DuckDB's arbitrary choice among tied rows was compared as a bag
-- argument: B's inner ORDER BY does not constrain the outer sort, so both sides keep one arbitrary row among those with the smallest b; Postgres returns a = 1 for A and a = 2 for B on t = {(0, 2, 2), (NULL, 1, 0), (1, 2, 0)}, and a = 2 for A once the same rows are inserted in another order
create table "t" ("id" INTEGER, "a" INTEGER, "b" INTEGER, unique ("id"));
SELECT "a" FROM "t" ORDER BY "b" LIMIT (1);
SELECT "a" FROM (SELECT "a", "b" FROM "t" ORDER BY "a" DESC) AS "s" ORDER BY "b" LIMIT (1);
