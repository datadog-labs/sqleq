-- Unless explicitly stated otherwise all files in this repository are licensed under the
-- Apache License Version 2.0.
-- This product includes software developed at Datadog (https://www.datadoghq.com/).
-- Copyright 2026-Present Datadog, Inc.

-- truth: equivalent
-- expect frontend: emit
-- expect fuzz: no-counterexample
-- expect qed: proved
-- expect sqlsolver-rust: proved
-- expect sqlsolver-jvm: proved
-- expect lean: unsupported
-- origin: the control beside joins/derived_table_right_numbered_from_base.sql (#29)
-- argument: the inputs of an inner join commute, and both sides project s.id
CREATE TABLE s (id INTEGER PRIMARY KEY, t_id INTEGER);
CREATE TABLE t (id INTEGER PRIMARY KEY, x INTEGER, c INTEGER);
SELECT s.id FROM s JOIN (SELECT * FROM t WHERE t.c = 1) AS d ON TRUE;
SELECT s.id FROM (SELECT * FROM t WHERE t.c = 1) AS d JOIN s ON TRUE;
