-- Unless explicitly stated otherwise all files in this repository are licensed under the
-- Apache License Version 2.0.
-- This product includes software developed at Datadog (https://www.datadoghq.com/).
-- Copyright 2026-Present Datadog, Inc.

-- truth: equivalent
-- expect frontend: emit-reflexive
-- expect fuzz: no-counterexample
-- expect qed: proved-literal
-- expect sqleq-solver: proved-literal
-- expect sqlsolver-jvm: proved-literal
-- expect lean: unsupported
-- origin: the control beside joins/using_after_right_join.sql: after a LEFT join the first binding
--   is the merged column, so this shape still lowers (#31)
-- argument: after a LEFT join the merged a is COALESCE(u.a, v.a), and v.a is non-NULL only where it
--   equals u.a, so it is u.a
CREATE TABLE u (a INTEGER);
CREATE TABLE v (a INTEGER);
CREATE TABLE w (a INTEGER);
SELECT 1 FROM u LEFT JOIN v USING (a) JOIN w USING (a);
SELECT 1 FROM u LEFT JOIN v USING (a) JOIN w ON u.a = w.a;
