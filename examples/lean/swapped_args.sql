-- Unless explicitly stated otherwise all files in this repository are licensed under the
-- Apache License Version 2.0.
-- This product includes software developed at Datadog (https://www.datadoghq.com/).
-- Copyright 2026-Present Datadog, Inc.

-- expect: unsupported
CREATE TABLE t (a int, b text);
INSERT INTO t (a, b) VALUES ($1, $2), ($3, $4);
INSERT INTO t (a, b) SELECT * FROM unnest($2::text[], $1::int[]);
