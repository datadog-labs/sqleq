-- Unless explicitly stated otherwise all files in this repository are licensed under the
-- Apache License Version 2.0.
-- This product includes software developed at Datadog (https://www.datadoghq.com/).
-- Copyright 2026-Present Datadog, Inc.

-- truth: not-equivalent
-- expect frontend: refuse:parameter-misaligned
-- expect fuzz: error
-- expect qed: no-plan
-- expect sqlsolver-rust: no-plan
-- expect sqlsolver-jvm: no-plan
-- expect lean: unsupported
-- binding: gather
-- origin: the Lean axis's fragment boundary: the unnest side casts each element to int, the VALUES
--   side assigns to bigint
-- witness: $1 = 3000000000, $2 = 'x', $3 = 1, $4 = 'y': A inserts both rows; B raises, because
--   3000000000 is out of range for integer

-- int4[] into a bigint column: the unnest side coerces each element, the VALUES side does not.
CREATE TABLE t (a bigint, b text);
INSERT INTO t (a, b) VALUES ($1, $2), ($3, $4);
INSERT INTO t (a, b) SELECT * FROM unnest($1::int[], $2::text[]);
