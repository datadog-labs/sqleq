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
-- origin: PR #209 review: the RANK() rewrite's guard: a window over a filtered table sees the filtered rows,
--   which the count over the whole table does not, so the window call is left to the lowering, which
--   refuses it
-- witness: t = {(-1), (1)}: A returns (1, 1); B returns (1, 2)
create table t (b integer);
SELECT b, RANK() OVER (ORDER BY b) AS r FROM t WHERE b > 0;
SELECT b, 1 + (SELECT COUNT(*) FROM t AS w WHERE w.b < t.b) AS r FROM t WHERE b > 0;
