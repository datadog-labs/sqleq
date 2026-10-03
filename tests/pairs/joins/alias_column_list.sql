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
-- origin: a table alias's column list was ignored, so `x.a` resolved to the table's own `a` (#31)
-- witness: t = {(1, 2)}: the column list renames t's columns in order, so A yields 2 and B 1
CREATE TABLE t (a INTEGER, b INTEGER);
SELECT x.a FROM t AS x(b, a);
SELECT x.a FROM t AS x;
