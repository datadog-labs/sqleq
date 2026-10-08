-- Unless explicitly stated otherwise all files in this repository are licensed under the
-- Apache License Version 2.0.
-- This product includes software developed at Datadog (https://www.datadoghq.com/).
-- Copyright 2026-Present Datadog, Inc.

-- truth: not-equivalent
-- expect frontend: refuse:unsupported
-- expect fuzz: nondet-skip
-- expect qed: no-plan
-- expect sqleq-solver: no-plan
-- expect lean: unsupported
-- origin: issue #124: the pair the issue reported, a LIMIT that leaves ties under a filter, which sqleq-fuzz
--   refuted by comparing the filtered rows' cardinality
-- witness: t = {(2, NULL, 2), (1, 2, 0), (0, 2, 1)}, inserted in that order: among the rows tied on g = 2,
--   Postgres 17 keeps (1, 2, 0) in A, the first inserted, and returns no row, and keeps (0, 2, 1) in B, the
--   first in the order of B's inner ORDER BY, and returns id 0. Inserted in reverse order, A returns id 0 too
-- The issue read the pair as equivalent: B's inner ORDER BY does not constrain the sort the LIMIT cuts, so
-- both sides would keep one arbitrary row among those tied on g. Since issue #59 was decided (#128), a
-- subquery's ORDER BY is observable to an order consumer above it: B's decides which tied row the LIMIT
-- keeps, so the pair is not equivalent, the frontend refuses it, and the counterexample sqleq-fuzz found
-- was a true one. sqleq-fuzz now skips the pair all the same: it cannot tell a difference an inner ORDER BY
-- makes from one the plan makes (nested_limit_ties_under_filter.sql), and a cut under a filter leaves it
-- nothing it can compare soundly.

create table "t" ("id" INTEGER, "g" INTEGER, "v" INTEGER, unique ("id"));
SELECT "id" FROM (SELECT * FROM "t" ORDER BY "g" LIMIT 1) AS "s" WHERE "v" = 1;
SELECT "id" FROM (SELECT * FROM (SELECT * FROM "t" ORDER BY "v" DESC) AS "x" ORDER BY "g" LIMIT 1) AS "s" WHERE "v" = 1;
