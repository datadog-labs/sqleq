-- Unless explicitly stated otherwise all files in this repository are licensed under the
-- Apache License Version 2.0.
-- This product includes software developed at Datadog (https://www.datadoghq.com/).
-- Copyright 2026-Present Datadog, Inc.

-- truth: equivalent
-- expect frontend: emit-reflexive
-- expect fuzz: no-counterexample
-- expect qed: proved-literal
-- expect sqlsolver-rust: proved-literal
-- expect sqlsolver-jvm: proved-literal
-- expect lean: unsupported
-- origin: issue #23: a parenthesized join in FROM was refused as an unsupported FROM factor (#31)
-- argument: parentheses around the whole FROM list group nothing
CREATE TABLE t (id INTEGER PRIMARY KEY, x INTEGER);
CREATE TABLE s (id INTEGER PRIMARY KEY, t_id INTEGER);
SELECT s.id FROM s JOIN t ON s.t_id = t.id;
SELECT s.id FROM (s JOIN t ON s.t_id = t.id);
