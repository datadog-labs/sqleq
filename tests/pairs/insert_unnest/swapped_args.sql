-- Unless explicitly stated otherwise all files in this repository are licensed under the
-- Apache License Version 2.0.
-- This product includes software developed at Datadog (https://www.datadoghq.com/).
-- Copyright 2026-Present Datadog, Inc.

-- truth: not-equivalent
-- expect frontend: refuse:parameter-misaligned
-- expect fuzz: error
-- expect qed: no-plan
-- expect sqleq-solver: no-plan
-- expect sqlsolver-jvm: no-plan
-- expect lean: unsupported
-- binding: gather
-- origin: the Lean axis's fragment boundary: the unnest arrays in the wrong order
-- witness: $1 = 1, $2 = 'x', $3 = 2, $4 = 'y': A inserts (1, x) and (2, y); B is rejected, because
--   its first column is text and a is integer
CREATE TABLE t (a int, b text);
INSERT INTO t (a, b) VALUES ($1, $2), ($3, $4);
INSERT INTO t (a, b) SELECT * FROM unnest($2::text[], $1::int[]);
