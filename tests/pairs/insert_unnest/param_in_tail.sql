-- Unless explicitly stated otherwise all files in this repository are licensed under the
-- Apache License Version 2.0.
-- This product includes software developed at Datadog (https://www.datadoghq.com/).
-- Copyright 2026-Present Datadog, Inc.

-- truth: equivalent
-- expect frontend: refuse:parameter-misaligned
-- expect fuzz: error
-- expect qed: no-plan
-- expect sqlsolver-rust: no-plan
-- expect sqlsolver-jvm: no-plan
-- expect lean: unsupported
-- binding: gather
-- origin: the Lean axis's fragment boundary: a parameter used both in a VALUES cell and in the
--   conflict tail
-- argument: a parameter outside the gathered arrays keeps its number on both sides, so both set b
--   to the same $3 on conflict; Postgres agrees on an empty table and on a second run over the
--   first one's rows

-- $3 is a VALUES cell on one side and the DO UPDATE value on the other.
CREATE TABLE t (a int PRIMARY KEY, b int);
INSERT INTO t (a, b) VALUES ($1, $2), ($3, $4) ON CONFLICT (a) DO UPDATE SET b = $3;
INSERT INTO t (a, b) SELECT * FROM unnest($1::int[], $2::int[]) ON CONFLICT (a) DO UPDATE SET b = $3;
