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
-- origin: issue #63: `FETCH FIRST ROW ONLY` (no count) escaped sqleq-fuzz's literal-LIMIT guard
-- witness: t = {(0, 2, 2), (NULL, 1, 0), (1, 2, 0)}, inserted in that order: among the rows tied on b = 0,
--   Postgres 17 keeps (NULL, 1, 0) in A, the first inserted, and returns a = 1, and keeps (1, 2, 0) in B, the
--   first in the order of B's inner ORDER BY, and returns a = 2. Inserted in reverse order, A returns a = 2 too
-- The pair was first pinned equivalent, reading the row kept among those tied on b as one arbitrary choice
-- on both sides. Since issue #59 was decided (#128), a subquery's ORDER BY is observable to an order consumer
-- above it, and Postgres hands the outer sort B's rows in the order of B's inner ORDER BY, which decides the
-- row kept. So the pair is not equivalent, and the frontend refuses it (issue #123). sqleq-fuzz compares a
-- FETCH at the top level by cardinality, and finds no counterexample.
create table "t" ("id" INTEGER, "a" INTEGER, "b" INTEGER, unique ("id"));
SELECT "a" FROM "t" ORDER BY "b" FETCH FIRST ROW ONLY;
SELECT "a" FROM (SELECT "a", "b" FROM "t" ORDER BY "a" DESC) AS "s" ORDER BY "b" FETCH FIRST ROW ONLY;
