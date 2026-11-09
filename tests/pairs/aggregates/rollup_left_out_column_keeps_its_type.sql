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
-- origin: PR #209 review: a grouping column a set leaves out became a bare NULL, an INTEGER, so COALESCE(a, 1)
--   over a numeric a divided integers in the total row; it is NULL of the column's type. The pair is
--   refused on both sides (a numeric division is observed), where it once was one plan
-- witness: t = {(3)}: A returns (1.5, 1), (0.5, 1); B returns (1.5, 1), (0, 1)
create table t (a numeric);
SELECT COALESCE(a, 1) / 2 AS h, count(*) AS n FROM t GROUP BY ROLLUP(a);
SELECT COALESCE(a, 1) / 2 AS h, count(*) AS n FROM t GROUP BY a UNION ALL SELECT COALESCE(NULL, 1) / 2 AS h, count(*) AS n FROM t;
