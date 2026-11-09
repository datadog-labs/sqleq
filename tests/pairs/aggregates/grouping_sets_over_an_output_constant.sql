-- Unless explicitly stated otherwise all files in this repository are licensed under the
-- Apache License Version 2.0.
-- This product includes software developed at Datadog (https://www.datadoghq.com/).
-- Copyright 2026-Present Datadog, Inc.

-- truth: not-equivalent
-- expect frontend: refuse:unsupported
-- expect fuzz: counterexample
-- expect qed: no-plan
-- expect sqleq-solver: no-plan
-- expect lean: unsupported
-- origin: PR #209 review: a grouping element that names the output alias of a constant is that
--   constant; the select list's copy of it was never recognized, so the total row kept the 5 where
--   Postgres makes it NULL. A grouping expression that reads no column of the query's own tables is
--   refused
-- witness: t = {(1)}: A returns (5, 1) and (NULL, 1); B returns (5, 1) twice
create table t (a integer);
SELECT 5 AS x, count(*) AS n FROM t GROUP BY ROLLUP(x);
SELECT 5 AS x, count(*) AS n FROM t GROUP BY x UNION ALL SELECT 5 AS x, count(*) AS n FROM t GROUP BY ();
