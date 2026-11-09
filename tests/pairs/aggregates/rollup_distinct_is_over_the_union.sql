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
-- origin: PR #209 review: a DISTINCT over grouping sets was kept on every branch of the expansion, merging
--   each set's rows alone; it is over the whole result, the UNION of the branches
-- witness: t = {(1)}: A returns one row, 1; B returns 1 twice
create table t (a integer);
SELECT DISTINCT count(*) AS n FROM t GROUP BY ROLLUP(a);
SELECT DISTINCT count(*) AS n FROM t GROUP BY a UNION ALL SELECT DISTINCT count(*) AS n FROM t;
