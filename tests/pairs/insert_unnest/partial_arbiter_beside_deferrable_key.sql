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
-- origin: issue #212: inference meets every unique index on the target's columns, so a deferrable constraint
--   on them beside the partial index is met too. A unique index cannot itself be deferrable
-- argument: Postgres never takes a deferrable constraint as an ON CONFLICT arbiter, so both sides raise 55000
--   before reading a row, on every run
CREATE TABLE w (a int, b text, c text, CONSTRAINT w_a UNIQUE (a) DEFERRABLE);
CREATE UNIQUE INDEX w_a_live ON w (a) WHERE b IS NULL;
INSERT INTO w (a, b, c) VALUES ($1, $2, $3), ($4, $5, $6) ON CONFLICT (a) WHERE b IS NULL DO UPDATE SET c = excluded.c;
INSERT INTO w (a, b, c) SELECT * FROM unnest($1::int[], $2::text[], $3::text[]) ON CONFLICT (a) WHERE b IS NULL DO UPDATE SET c = excluded.c;
