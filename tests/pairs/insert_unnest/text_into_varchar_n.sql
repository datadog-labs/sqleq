-- Unless explicitly stated otherwise all files in this repository are licensed under the
-- Apache License Version 2.0.
-- This product includes software developed at Datadog (https://www.datadoghq.com/).
-- Copyright 2026-Present Datadog, Inc.

-- truth: equivalent
-- expect frontend: refuse:parameter-misaligned
-- expect fuzz: not-comparable
-- expect qed: no-plan
-- expect sqleq-solver: no-plan
-- expect sqlsolver-jvm: no-plan
-- expect lean: proved-gather
-- binding: gather
-- origin: the Lean axis required each unnest element type to be its column's own; text[] holds every
--   varchar, and both sides then make the same assignment to varchar(8)

-- text[] into a varchar(8) column. The VALUES side's parameter is a varchar, and both sides assign
-- the same string to varchar(8): a ninth character raises on both, and trailing spaces past the
-- eighth are dropped on both. (A cast to varchar(8)[] would truncate instead: see
-- varchar_cast_truncates.sql.)
CREATE TABLE tags (id bigint PRIMARY KEY, label varchar(8) NOT NULL);
INSERT INTO tags (id, label) VALUES ($1, $2), ($3, $4);
INSERT INTO tags (id, label) SELECT * FROM unnest($1::bigint[], $2::text[]);
