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
-- origin: issue #123: the same pair with a call lowering refuses, which the reflexivity check credited because
--   it stripped both subquery ORDER BYs
-- witness: u = {(1, 2.0), (2, 2.00)}: A yields ('2.0', false) and B yields ('2.00', false) (Postgres 17, max_parallel_workers_per_gather = 0)
create table "u" ("k" INTEGER, "n" NUMERIC);
SELECT CAST(x.n AS TEXT), random() > 2 FROM (SELECT DISTINCT n FROM (SELECT n FROM u ORDER BY k) s) x;
SELECT CAST(x.n AS TEXT), random() > 2 FROM (SELECT DISTINCT n FROM (SELECT n FROM u ORDER BY k DESC) s) x;
