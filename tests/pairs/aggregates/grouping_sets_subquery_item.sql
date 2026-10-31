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
-- origin: PR #209 review: the expansion matches grouping columns by text with no scope, so a subquery in a
--   select item, whose own columns the match could take for grouping columns, is refused
-- witness: t = {(1)}, u = {}: A returns (1, 0), (NULL, 0); B returns (1, 0)
create table t (a integer);
create table u (x integer);
SELECT a, (SELECT count(*) FROM u WHERE u.x = t.a) AS c FROM t GROUP BY ROLLUP(a);
SELECT a, (SELECT count(*) FROM u WHERE u.x = t.a) AS c FROM t GROUP BY a;
