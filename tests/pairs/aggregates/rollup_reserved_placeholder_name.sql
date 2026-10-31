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
-- origin: PR #209 review: a column named like the expansion's aggregate placeholder made the frontend panic
--   (index out of bounds); every identifier beginning with __sqleq_ is refused before any rewrite
--   (src/lower.rs, refuse_reserved_identifiers)
-- witness: t = {(1, 1)}: A returns (1, 1), (NULL, 1); B returns (1, 1)
create table t (a integer, __sqleq_agg_0 integer);
SELECT __sqleq_agg_0 AS v, count(*) AS n FROM t GROUP BY ROLLUP(__sqleq_agg_0);
SELECT __sqleq_agg_0 AS v, count(*) AS n FROM t GROUP BY __sqleq_agg_0;
