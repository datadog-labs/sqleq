-- Unless explicitly stated otherwise all files in this repository are licensed under the
-- Apache License Version 2.0.
-- This product includes software developed at Datadog (https://www.datadoghq.com/).
-- Copyright 2026-Present Datadog, Inc.

-- truth: not-equivalent
-- expect frontend: refuse:unsupported
-- expect fuzz: not-comparable
-- expect qed: no-plan
-- expect sqleq-solver: no-plan
-- expect lean: unsupported
-- origin: issue #123: a subquery ORDER BY the lowering drops decides which of two values = calls equal a
--   DISTINCT, GROUP BY, UNION, DISTINCT ON or max keeps, and the two queries lowered to one plan, which lifted
--   the refusal of a read that tells the two apart
-- witness: u = {(1, 'a'), (2, 'A')}: under ci 'a' = 'A', so A yields 97 and B yields 65 (Postgres 17, max_parallel_workers_per_gather = 0)
create collation "ci" (provider = icu, locale = 'und-u-ks-level2', deterministic = false);
create table "u" ("k" INTEGER, "n" TEXT COLLATE "ci");
SELECT ascii(x.n) FROM (SELECT DISTINCT n FROM (SELECT n FROM u ORDER BY k) s) x;
SELECT ascii(x.n) FROM (SELECT DISTINCT n FROM (SELECT n FROM u ORDER BY k DESC) s) x;
