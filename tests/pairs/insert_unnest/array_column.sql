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
-- origin: the Lean axis's fragment boundary: no gather yields an array-typed column's value
-- witness: $1 = 1, $2 = '{x,y}': A inserts (1, {x,y}); B, under the gather binding, is rejected,
--   because unnest of a text[] yields text and tags is text[]

-- unnest flattens every dimension, so no gather produces an array-typed column's value.
CREATE TABLE t (a int, tags text[]);
INSERT INTO t (a, tags) VALUES ($1, $2);
INSERT INTO t (a, tags) SELECT * FROM unnest($1::int[], $2::text[]);
