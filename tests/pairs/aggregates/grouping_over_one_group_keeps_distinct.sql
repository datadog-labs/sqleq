-- Unless explicitly stated otherwise all files in this repository are licensed under the
-- Apache License Version 2.0.
-- This product includes software developed at Datadog (https://www.datadoghq.com/).
-- Copyright 2026-Present Datadog, Inc.

-- truth: not-equivalent
-- expect frontend: emit
-- expect fuzz: counterexample
-- expect qed: no-proof
-- expect sqleq-solver: no-proof
-- expect cvc5: error
-- expect lean: unsupported
-- origin: PR #209 review: a plain GROUP BY that reads GROUPING(..) is expanded as its one grouping set,
--   and lost its DISTINCT as the single set of GROUPING SETS ((a)) did
-- witness: t = {(1), (2)}: A returns 0 once, B returns 0 twice
create table t (a integer);
SELECT DISTINCT GROUPING(a) AS g FROM t GROUP BY a;
SELECT 0 AS g FROM t GROUP BY a;
