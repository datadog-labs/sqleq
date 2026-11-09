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
-- origin: PR #209 review, the twin of rollup_single_set_keeps_distinct: a single grouping set with
--   DISTINCT, as the plain GROUP BY with DISTINCT it is
-- argument: GROUPING SETS ((a)) groups by a, as GROUP BY a does
create table t (a integer);
SELECT DISTINCT count(*) AS n FROM t GROUP BY GROUPING SETS ((a));
SELECT DISTINCT count(*) AS n FROM t GROUP BY a;
