-- Unless explicitly stated otherwise all files in this repository are licensed under the
-- Apache License Version 2.0.
-- This product includes software developed at Datadog (https://www.datadoghq.com/).
-- Copyright 2026-Present Datadog, Inc.

-- truth: not-equivalent
-- expect frontend: refuse:unsupported
-- expect fuzz: counterexample
-- expect qed: no-plan
-- expect sqleq-solver: no-plan
-- expect sqlsolver-jvm: no-plan
-- expect lean: unsupported
-- origin: a bare `*` over JOIN ... USING lowered as the ON form, which has one more column (#31)
-- witness: t = {(1, 2)}: A returns (1, 2, 2), with a once; B returns (1, 2, 1, 2)
CREATE TABLE t (a INTEGER, b INTEGER);
SELECT * FROM t AS x JOIN t AS y USING (a);
SELECT * FROM t AS x JOIN t AS y ON x.a = y.a;
