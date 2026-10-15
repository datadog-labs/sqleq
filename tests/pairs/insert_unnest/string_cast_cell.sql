-- Unless explicitly stated otherwise all files in this repository are licensed under the
-- Apache License Version 2.0.
-- This product includes software developed at Datadog (https://www.datadoghq.com/).
-- Copyright 2026-Present Datadog, Inc.

-- truth: equivalent
-- expect frontend: refuse:parameter-misaligned
-- expect fuzz: error
-- expect qed: no-plan
-- expect sqleq-solver: no-plan
-- expect sqlsolver-jvm: no-plan
-- expect lean: proved-gather
-- binding: gather
-- origin: a VALUES cell casts its parameter through a string type, which is the target type's input
-- argument: a cast from text or varchar to uuid or to an enum is the type's input function, the same
--   parse the unnest side's array elements get, so A's cells hold what B's arrays do
CREATE TYPE status AS ENUM ('new', 'done');
CREATE TABLE tk (id uuid PRIMARY KEY, s status);
INSERT INTO tk (id, s) VALUES (CAST($1::varchar AS uuid), $2::text::status), (CAST($3::varchar AS uuid), $4::text::status);
INSERT INTO tk (id, s) SELECT * FROM unnest($1::uuid[], $2::status[]);
