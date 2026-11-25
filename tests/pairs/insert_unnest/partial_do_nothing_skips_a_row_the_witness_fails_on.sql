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
-- origin: #300's review, a known gap: under DO NOTHING with only a partial arbiter the witness has no arbiter,
--   so the same row twice collides on the primary key. Postgres asks the arbiter first: deleted_at is NULL
--   in both rows, the second is skipped, and the key is never looked at. The witness is withheld, which
--   costs credit, and its reason says the failure is the witness's and not the statement's
-- argument: both sides name the same arbiter, the partial unique index on a where deleted_at is null; under
--   the gather rule they propose the same rows in the same order, so the same rows conflict and are skipped
CREATE TABLE w (id int PRIMARY KEY, a int, deleted_at timestamp);
CREATE UNIQUE INDEX w_a_live ON w (a) WHERE deleted_at IS NULL;
INSERT INTO w (id, a) VALUES ($1, $2), ($1, $2) ON CONFLICT (a) WHERE deleted_at IS NULL DO NOTHING;
INSERT INTO w (id, a) SELECT * FROM unnest($1::int[], $2::int[]) ON CONFLICT (a) WHERE deleted_at IS NULL DO NOTHING;
