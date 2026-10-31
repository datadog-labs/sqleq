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
-- origin: PR #209 review: the RANK() rewrite walked into the subqueries of a select item and rewrote a RANK()
--   over another table against the outer one, so this pair was one plan; an item with a subquery in
--   it is now left alone, and the window call is refused (src/normalize.rs, lower_rank_windows)
-- witness: t = u = {(1), (2)}: A returns (1, 1), (2, 1); B returns (1, 1), (2, 2)
create table t (b integer);
create table u (b integer);
SELECT b, (SELECT RANK() OVER (ORDER BY b) FROM u WHERE u.b = t.b) AS r FROM t;
SELECT b, (SELECT 1 + (SELECT COUNT(*) FROM t AS w WHERE (((w.b < t.b)) OR (t.b IS NULL AND w.b IS NOT NULL) OR (w.b IS NOT DISTINCT FROM t.b AND FALSE))) FROM u WHERE u.b = t.b) AS r FROM t;
