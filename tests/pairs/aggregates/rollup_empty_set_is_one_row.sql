-- Unless explicitly stated otherwise all files in this repository are licensed under the
-- Apache License Version 2.0.
-- This product includes software developed at Datadog (https://www.datadoghq.com/).
-- Copyright 2026-Present Datadog, Inc.

-- truth: not-equivalent
-- expect frontend: emit
-- expect fuzz: counterexample
-- expect qed: no-proof
-- expect sqleq-solver: no-proof
-- expect cvc5: model
-- expect lean: unsupported
-- origin: PR #209 review: the empty grouping set of a ROLLUP was expanded as an empty GROUP BY, a plain
--   SELECT where no aggregate made the branch group, so one row per input row; it is GROUP BY (),
--   one group of every row and exactly one row (src/lower.rs, expand_grouping_sets)
-- witness: t = {(1), (2)}: A returns 1, 2, NULL; B returns 1, 2, NULL, NULL
create table t (a integer);
SELECT a FROM t GROUP BY ROLLUP(a);
SELECT a FROM t GROUP BY a UNION ALL SELECT NULL AS a FROM t;
