-- Unless explicitly stated otherwise all files in this repository are licensed under the
-- Apache License Version 2.0.
-- This product includes software developed at Datadog (https://www.datadoghq.com/).
-- Copyright 2026-Present Datadog, Inc.

-- expect: invalid-sql
-- One parameter per row where the syntax needs one per column: Postgres rejects the VALUES side.
CREATE TABLE t (a int, b text, c int);
INSERT INTO t (a, b, c) VALUES ($1), ($2), ($3);
INSERT INTO t (a, b, c) SELECT * FROM unnest($1::int[], $2::text[], $3::int[]);
