-- Unless explicitly stated otherwise all files in this repository are licensed under the
-- Apache License Version 2.0.
-- This product includes software developed at Datadog (https://www.datadoghq.com/).
-- Copyright 2026-Present Datadog, Inc.

-- truth: not-equivalent
-- expect frontend: refuse:unsupported
-- expect fuzz: counterexample
-- expect qed: no-plan
-- expect sqleq-solver: no-plan
-- expect cvc5: no-plan
-- expect lean: unsupported
-- origin: PR #209 review: DISTINCT ON beside grouping sets is refused, as the expansion has no branch to put it
--   on
-- witness: t = {(1, 2)}: A returns (1, 1), (NULL, 1); B returns (1, 1)
create table t (a integer, b integer);
SELECT DISTINCT ON (a) a, count(*) AS n FROM t GROUP BY ROLLUP(a, b);
SELECT a, count(*) AS n FROM t GROUP BY a;
