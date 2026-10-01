-- Unless explicitly stated otherwise all files in this repository are licensed under the
-- Apache License Version 2.0.
-- This product includes software developed at Datadog (https://www.datadoghq.com/).
-- Copyright 2026-Present Datadog, Inc.

-- expect: proved-gather
-- A column with a length and an unnest cast without one: both sides reach varchar(8) through the
-- same assignment coercion, which raises on a long value rather than truncating it.
CREATE TABLE t (a int, code varchar(8));
INSERT INTO t (a, code) VALUES ($1, $2);
INSERT INTO t (a, code) SELECT * FROM unnest($1::int[], $2::varchar[]);
