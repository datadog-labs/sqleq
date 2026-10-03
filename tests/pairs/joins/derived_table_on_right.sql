-- Unless explicitly stated otherwise all files in this repository are licensed under the
-- Apache License Version 2.0.
-- This product includes software developed at Datadog (https://www.datadoghq.com/).
-- Copyright 2026-Present Datadog, Inc.

-- truth: equivalent
-- expect frontend: emit
-- expect fuzz: no-counterexample
-- expect qed: proved
-- expect sqleq-solver: proved
-- expect sqlsolver-jvm: proved
-- expect lean: unsupported
-- origin: issue #22: a derived table on the right of a join was refused as correlated by both
--   SQLSolver bridges, though QED proved the pair (#29)
-- argument: `t.x = 1` and `1 = t.x` are the same predicate
CREATE TABLE s (id INTEGER PRIMARY KEY, t_id INTEGER);
CREATE TABLE t (id INTEGER PRIMARY KEY, x INTEGER);
SELECT s.id FROM s JOIN (SELECT * FROM t AS t WHERE t.x = 1) AS t ON s.t_id = t.id;
SELECT s.id FROM s JOIN (SELECT * FROM t AS t WHERE 1 = t.x) AS t ON s.t_id = t.id;
