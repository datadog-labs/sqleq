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
-- origin: PR #209 review: a ROLLUP nested in GROUPING SETS was read as a call of a function named rollup and
--   grouped by; it is refused
-- witness: t = {(1, 2)}: A returns (1, NULL, 1), (NULL, 2, 1), (NULL, NULL, 1); B returns (1, 2, 1)
create table t (a integer, b integer);
SELECT a, b, count(*) AS n FROM t GROUP BY GROUPING SETS ((a), ROLLUP(b));
SELECT a, b, count(*) AS n FROM t GROUP BY a, b;
