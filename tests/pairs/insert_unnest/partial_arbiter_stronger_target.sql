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
-- expect lean: proved-gather
-- binding: gather
-- origin: issue #212: a target predicate that says more than the index's still implies it, so Postgres infers
--   the index
-- argument: b IS NULL AND c IS NOT NULL implies the index's b IS NULL, so both sides name the partial index
--   as the arbiter; under the gather rule they propose the same rows in the same order
CREATE TABLE w (a int, b text, c text);
CREATE UNIQUE INDEX w_a_live ON w (a) WHERE b IS NULL;
INSERT INTO w (a, b, c) VALUES ($1, $2, $3), ($4, $5, $6)
  ON CONFLICT (a) WHERE b IS NULL AND c IS NOT NULL DO UPDATE SET c = excluded.c;
INSERT INTO w (a, b, c) SELECT * FROM unnest($1::int[], $2::text[], $3::text[])
  ON CONFLICT (a) WHERE b IS NULL AND c IS NOT NULL DO UPDATE SET c = excluded.c;
