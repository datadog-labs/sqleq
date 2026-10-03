-- Unless explicitly stated otherwise all files in this repository are licensed under the
-- Apache License Version 2.0.
-- This product includes software developed at Datadog (https://www.datadoghq.com/).
-- Copyright 2026-Present Datadog, Inc.

-- truth: equivalent
-- expect frontend: refuse:parameter-misaligned
-- expect fuzz: nondet-skip
-- expect qed: no-plan
-- expect sqleq-solver: no-plan
-- expect sqlsolver-jvm: no-plan
-- expect lean: proved-gather-generated
-- binding: gather-generated
-- origin: a statement clock and a random uuid as VALUES cells, under a unique key

-- The omitted bigserial id is drawn the same way on both sides, so RETURNING agrees.
CREATE TABLE sessions (id bigserial PRIMARY KEY, started timestamptz NOT NULL, token uuid UNIQUE, agent text);
INSERT INTO sessions (started, token, agent) VALUES (now(), gen_random_uuid(), $1), (now(), gen_random_uuid(), $2) RETURNING id, token;
INSERT INTO sessions (started, token, agent) SELECT * FROM unnest($1::timestamptz[], $2::uuid[], $3::text[]) RETURNING id, token;
