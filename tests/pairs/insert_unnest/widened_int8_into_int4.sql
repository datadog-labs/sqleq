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
-- origin: the Lean axis required each unnest element type to be its column's own; an element type
--   that holds every value of the column's type is admitted, checked by the kernel too

-- bigint[] into an integer column. The VALUES side's parameter takes the column's type, integer,
-- and every integer is a bigint that assigns back to the same integer. (The other way round,
-- int[] into a bigint column, is not equivalent: see cast_mismatch.sql.)
CREATE TABLE events (id int PRIMARY KEY, kind text);
INSERT INTO events (id, kind) VALUES ($1, $2), ($3, $4);
INSERT INTO events (id, kind) SELECT * FROM unnest($1::bigint[], $2::text[]);
