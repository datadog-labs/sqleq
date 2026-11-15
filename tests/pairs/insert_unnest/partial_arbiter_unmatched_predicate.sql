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
-- origin: issue #212: a target predicate that does not imply the index's infers no arbiter, and a proof that
--   may be vacuous is not credited
-- argument: b IS NOT NULL does not imply the index's b IS NULL and no other unique index is on a, so both
--   sides raise before reading a row (there is no unique or exclusion constraint matching the ON CONFLICT
--   specification)
CREATE TABLE w (a int, b text, c text);
CREATE UNIQUE INDEX w_a_live ON w (a) WHERE b IS NULL;
INSERT INTO w (a, b, c) VALUES ($1, $2, $3), ($4, $5, $6)
  ON CONFLICT (a) WHERE b IS NOT NULL DO UPDATE SET c = excluded.c;
INSERT INTO w (a, b, c) SELECT * FROM unnest($1::int[], $2::text[], $3::text[])
  ON CONFLICT (a) WHERE b IS NOT NULL DO UPDATE SET c = excluded.c;
