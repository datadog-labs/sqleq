-- Unless explicitly stated otherwise all files in this repository are licensed under the
-- Apache License Version 2.0.
-- This product includes software developed at Datadog (https://www.datadoghq.com/).
-- Copyright 2026-Present Datadog, Inc.

-- expect: no-witness
-- `owner` is NOT NULL with no default and neither side supplies it: every run fails.
CREATE TABLE docs (id bigserial PRIMARY KEY, owner int NOT NULL, title text);
INSERT INTO docs (title) VALUES ($1), ($2);
INSERT INTO docs (title) SELECT * FROM unnest($1::text[]);
