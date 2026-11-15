-- Unless explicitly stated otherwise all files in this repository are licensed under the
-- Apache License Version 2.0.
-- This product includes software developed at Datadog (https://www.datadoghq.com/).
-- Copyright 2026-Present Datadog, Inc.

-- truth: equivalent
-- expect frontend: refuse:parameter-misaligned
-- expect fuzz: no-counterexample-gather
-- expect qed: no-plan
-- expect sqleq-solver: no-plan
-- expect cvc5: no-plan
-- expect lean: no-witness
-- binding: gather
-- origin: issue #212, a known gap: the witness reads a partial index as total, so the same row twice
--   collides in it although Postgres, with non-NULL parameters, puts neither in the index and inserts both.
--   The proof holds; it is the witness that is withheld, which costs credit and claims nothing
-- argument: both sides name the same arbiter, the partial unique index on a where b is null; under the gather
--   rule they propose the same rows in the same order, so the same rows conflict and are skipped
CREATE TABLE w (a int, b text);
CREATE UNIQUE INDEX w_a_live ON w (a) WHERE b IS NULL;
INSERT INTO w (a, b) VALUES ($1, $2), ($1, $2) ON CONFLICT (a) WHERE b IS NULL DO NOTHING;
INSERT INTO w (a, b) SELECT * FROM unnest($1::int[], $2::text[]) ON CONFLICT (a) WHERE b IS NULL DO NOTHING;
