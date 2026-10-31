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
-- origin: PR #209 review: the grouping expressions were matched bottom-up, so in the set that keeps a / 10
--   and not a the a inside a / 10 was nulled first; they are matched from the top of each expression
--   down, and the whole a / 10 is kept
-- witness: t = {(15)}: A returns (1, 1), (1, 1), (NULL, 1); B returns (1, 1), (NULL, 1), (NULL, 1)
create table t (a integer);
SELECT a / 10 AS bucket, count(*) AS n FROM t GROUP BY ROLLUP(a / 10, a);
SELECT a / 10 AS bucket, count(*) AS n FROM t GROUP BY a / 10, a UNION ALL SELECT NULL / 10 AS bucket, count(*) AS n FROM t GROUP BY a / 10 UNION ALL SELECT NULL / 10 AS bucket, count(*) AS n FROM t;
