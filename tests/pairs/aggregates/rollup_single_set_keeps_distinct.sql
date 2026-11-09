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
-- origin: PR #209 review: the expansion kept DISTINCT only where it joined two or more branches with
--   UNION, so a single grouping set lost it
-- witness: t = {(1), (2)}: A returns the one row {1}, B returns 1 twice
create table t (a integer);
SELECT DISTINCT count(*) AS n FROM t GROUP BY GROUPING SETS ((a));
SELECT count(*) AS n FROM t GROUP BY a;
