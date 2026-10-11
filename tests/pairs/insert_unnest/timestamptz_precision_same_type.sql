-- Unless explicitly stated otherwise all files in this repository are licensed under the
-- Apache License Version 2.0.
-- This product includes software developed at Datadog (https://www.datadoghq.com/).
-- Copyright 2026-Present Datadog, Inc.

-- truth: equivalent
-- expect frontend: refuse:parameter-misaligned
-- expect fuzz: no-counterexample-gather
-- expect qed: no-plan
-- expect sqleq-solver: no-plan
-- expect sqlsolver-jvm: no-plan
-- expect lean: proved-gather
-- binding: gather
-- origin: the control for reading `timestamp(3) with time zone` as `timestamptz` with a precision:
--   timestamptz[] is the column's own base type, and both sides round to the precision alike

-- timestamptz[] into a timestamptz(3) column: the VALUES side's parameter is a timestamptz too, and
-- both sides reach the column through the same rounding to three digits.
CREATE TABLE events (id int PRIMARY KEY, at timestamp(3) with time zone);
INSERT INTO events (id, at) VALUES ($1, $2), ($3, $4);
INSERT INTO events (id, at) SELECT * FROM unnest($1::int[], $2::timestamptz[]);
