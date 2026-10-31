-- Unless explicitly stated otherwise all files in this repository are licensed under the
-- Apache License Version 2.0.
-- This product includes software developed at Datadog (https://www.datadoghq.com/).
-- Copyright 2026-Present Datadog, Inc.

-- truth: equivalent
-- expect frontend: emit-reflexive
-- expect fuzz: no-counterexample
-- expect qed: proved-literal
-- expect sqleq-solver: proved-literal
-- expect cvc5: proved-literal
-- expect lean: unsupported
-- origin: PR #209 review: GROUPING(a) under a ROLLUP, written out as the constant each set gives it
-- argument: GROUPING(a) is 0 in a set that groups by a and 1 in one that leaves it out
create table t (a integer);
SELECT a, GROUPING(a) AS g, count(*) AS n FROM t GROUP BY ROLLUP(a);
SELECT a, 0 AS g, count(*) AS n FROM t GROUP BY a UNION ALL SELECT NULL AS a, 1 AS g, count(*) AS n FROM t GROUP BY ();
