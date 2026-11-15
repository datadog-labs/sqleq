-- Unless explicitly stated otherwise all files in this repository are licensed under the
-- Apache License Version 2.0.
-- This product includes software developed at Datadog (https://www.datadoghq.com/).
-- Copyright 2026-Present Datadog, Inc.

-- truth: equivalent
-- expect frontend: refuse:parameter-misaligned
-- expect fuzz: error
-- expect qed: no-plan
-- expect sqleq-solver: no-plan
-- expect cvc5: no-plan
-- expect lean: no-witness
-- binding: gather
-- origin: issue #212: under DO NOTHING a partial arbiter skips only a row its predicate holds of. The witness
--   reads a partial index as total and would skip the second row here, where Postgres raises on another key,
--   so with only a partial arbiter it is asked for a run with no collision
-- argument: with non-NULL parameters no row is one the partial index covers, and the second row collides
--   with the first on k, which both take from its default, so both sides raise a unique violation on every
--   such run
CREATE TABLE w (a int, b text, k int DEFAULT 0);
CREATE UNIQUE INDEX w_a_live ON w (a) WHERE b IS NULL;
CREATE UNIQUE INDEX w_k ON w (k);
INSERT INTO w (a, b) VALUES ($1, $2), ($1, $2) ON CONFLICT (a) WHERE b IS NULL DO NOTHING;
INSERT INTO w (a, b) SELECT * FROM unnest($1::int[], $2::text[]) ON CONFLICT (a) WHERE b IS NULL DO NOTHING;
