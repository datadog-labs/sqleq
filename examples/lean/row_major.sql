-- Unless explicitly stated otherwise all files in this repository are licensed under the
-- Apache License Version 2.0.
-- This product includes software developed at Datadog (https://www.datadoghq.com/).
-- Copyright 2026-Present Datadog, Inc.

-- expect: proved-gather
-- A three-row insert rewritten as one unnest of three-element arrays: under the gather rule
-- $1 is (the first column of every row), so unnest yields the same rows in the same order.
CREATE TABLE events (id bigint NOT NULL, kind text, at timestamptz);
INSERT INTO events (id, kind, at) VALUES ($1, $2, $3), ($4, $5, $6), ($7, $8, $9);
INSERT INTO events (id, kind, at) SELECT * FROM unnest($1::bigint[], $2::text[], $3::timestamptz[]);
