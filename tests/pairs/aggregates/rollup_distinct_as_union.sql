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
-- origin: PR #209 review: DISTINCT over grouping sets, written out as the UNION of the branches
-- argument: SELECT DISTINCT over the union of the grouping sets' rows is their UNION, which merges
--   across the sets as well as within one
create table t (a integer);
SELECT DISTINCT count(*) AS n FROM t GROUP BY ROLLUP(a);
SELECT count(*) AS n FROM t GROUP BY a UNION SELECT count(*) AS n FROM t GROUP BY ();
