-- Unless explicitly stated otherwise all files in this repository are licensed under the
-- Apache License Version 2.0.
-- This product includes software developed at Datadog (https://www.datadoghq.com/).
-- Copyright 2026-Present Datadog, Inc.

-- expect: unsupported
-- unnest flattens every dimension, so no gather produces an array-typed column's value.
CREATE TABLE t (a int, tags text[]);
INSERT INTO t (a, tags) VALUES ($1, $2);
INSERT INTO t (a, tags) SELECT * FROM unnest($1::int[], $2::text[]);
