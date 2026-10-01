-- Unless explicitly stated otherwise all files in this repository are licensed under the
-- Apache License Version 2.0.
-- This product includes software developed at Datadog (https://www.datadoghq.com/).
-- Copyright 2026-Present Datadog, Inc.

-- expect: unsupported
-- int4[] into a bigint column: the unnest side coerces each element, the VALUES side does not.
CREATE TABLE t (a bigint, b text);
INSERT INTO t (a, b) VALUES ($1, $2), ($3, $4);
INSERT INTO t (a, b) SELECT * FROM unnest($1::int[], $2::text[]);
