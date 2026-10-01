-- Unless explicitly stated otherwise all files in this repository are licensed under the
-- Apache License Version 2.0.
-- This product includes software developed at Datadog (https://www.datadoghq.com/).
-- Copyright 2026-Present Datadog, Inc.

-- expect: proved-gather
-- The same tuple twice, but the only key is the omitted serial id, so each row gets its own id.
CREATE TABLE log (id serial PRIMARY KEY, msg text, level int);
INSERT INTO log (msg, level) VALUES ($1, $2), ($1, $2);
INSERT INTO log (msg, level) SELECT * FROM unnest($1::text[], $2::int[]);
