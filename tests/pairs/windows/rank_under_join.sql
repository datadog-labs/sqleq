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
-- origin: PR #209 review: the RANK() rewrite's guard: a window over a join sees the joined rows, so the window
--   call is left to the lowering, which refuses it
-- witness: t = {(1), (2)}, u = {(2), (2)}: A returns (2, 1), (2, 1); B returns (2, 2), (2, 2)
create table t (b integer);
create table u (b integer);
SELECT t.b, RANK() OVER (ORDER BY t.b) AS r FROM t JOIN u ON u.b = t.b;
SELECT t.b, 1 + (SELECT COUNT(*) FROM t AS w WHERE w.b < t.b) AS r FROM t JOIN u ON u.b = t.b;
