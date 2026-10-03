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
-- origin: the control beside joins/alias_column_list.sql (#31)
-- argument: the column list renames t's columns in order, so x.b is t's first column, a
CREATE TABLE t (a INTEGER, b INTEGER);
SELECT x.b FROM t AS x(b, a);
SELECT a FROM t;
