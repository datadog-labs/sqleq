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
-- origin: issue #123: the same for an outer DISTINCT ON with no ORDER BY of its own, which keeps the first row
--   of each key in the order a sorted subquery hands them on
-- witness: t = {(1, 1), (2, 1)}: A returns 1 and B returns 2 (Postgres 17)
create table "t" ("id" INTEGER, "g" INTEGER);
SELECT DISTINCT ON (g) id FROM (SELECT g, id FROM t ORDER BY id) s;
SELECT DISTINCT ON (g) id FROM (SELECT g, id FROM t ORDER BY id DESC) s;
