-- Unless explicitly stated otherwise all files in this repository are licensed under the
-- Apache License Version 2.0.
-- This product includes software developed at Datadog (https://www.datadoghq.com/).
-- Copyright 2026-Present Datadog, Inc.

-- truth: not-equivalent
-- expect frontend: emit
-- expect fuzz: counterexample
-- expect qed: no-proof
-- expect sqleq-solver: no-proof
-- expect sqlsolver-jvm: no-proof
-- expect lean: unsupported
-- origin: issue #22: both SQLSolver bridges numbered a join's right input from after its left, so
--   `t.c` on the right read as `t.id` and this pair was proved (#29)
-- witness: s = {(1, NULL)}, t = {(1, NULL, 0)}: A's derived table is empty, so A returns no row;
--   B's holds t's row, so B returns 1

-- QED and the frontend number a join's right input from the enclosing base, so `t.c` is t's
-- third column wherever the derived table sits.
CREATE TABLE s (id INTEGER PRIMARY KEY, t_id INTEGER);
CREATE TABLE t (id INTEGER PRIMARY KEY, x INTEGER, c INTEGER);
SELECT s.id FROM s JOIN (SELECT * FROM t WHERE t.c = 1) AS d ON TRUE;
SELECT s.id FROM (SELECT * FROM t WHERE t.id = 1) AS d JOIN s ON TRUE;
