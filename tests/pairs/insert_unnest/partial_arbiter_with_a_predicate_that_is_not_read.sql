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
-- origin: #300's review: an index predicate that is not IS [NOT] NULL tests joined by AND is not read, and was
--   reported as no arbiter, a statement that fails. Postgres infers this index, since b IS NULL implies
--   b IS NULL OR c IS NULL, and the statement runs. The pair gets no witness, for the reason that the
--   predicate was not read
-- argument: both sides name the same arbiter, the partial unique index on a where b or c is null; under the
--   gather rule they propose the same rows in the same order, so the same rows conflict and are updated
CREATE TABLE w (a int, b text, c text);
CREATE UNIQUE INDEX w_a_some ON w (a) WHERE b IS NULL OR c IS NULL;
INSERT INTO w (a, b, c) VALUES ($1, $2, $3), ($4, $5, $6) ON CONFLICT (a) WHERE b IS NULL DO UPDATE SET c = excluded.c;
INSERT INTO w (a, b, c) SELECT * FROM unnest($1::int[], $2::text[], $3::text[]) ON CONFLICT (a) WHERE b IS NULL DO UPDATE SET c = excluded.c;
