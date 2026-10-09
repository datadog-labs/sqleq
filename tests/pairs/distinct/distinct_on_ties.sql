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
-- origin: issue #89: sqleq-fuzz compared the row DuckDB kept among rows tied inside a DISTINCT ON key as
--   a bag, though Postgres chooses it by physical order
-- witness: t = {(2, NULL, 2), (1, 2, 0), (0, 2, 1)}, inserted in that order: for g = 2, Postgres 17 keeps
--   v = 0 in A, the first of the tied rows inserted, and v = 1 in B, the first in the order of B's inner
--   ORDER BY. Inserted in reverse order, A keeps v = 1 too
-- The pair was first pinned equivalent, reading the row kept among those tied on g as one arbitrary choice
-- on both sides. Since issue #59 was decided (#128), a subquery's ORDER BY is observable to an order consumer
-- above it, and Postgres hands DISTINCT ON B's rows in the order of B's inner ORDER BY, which decides the
-- row kept. So the pair is not equivalent, and the frontend refuses it (issue #123). sqleq-fuzz compares a
-- DISTINCT ON at the top level by its cardinality, the number of keys, and finds no counterexample.

create table "t" ("id" INTEGER, "g" INTEGER, "v" INTEGER, unique ("id"));
SELECT DISTINCT ON ("g") "g", "v" FROM "t" ORDER BY "g";
SELECT DISTINCT ON ("g") "g", "v" FROM (SELECT * FROM "t" ORDER BY "v" DESC) AS "s" ORDER BY "g";
