-- Unless explicitly stated otherwise all files in this repository are licensed under the
-- Apache License Version 2.0.
-- This product includes software developed at Datadog (https://www.datadoghq.com/).
-- Copyright 2026-Present Datadog, Inc.

-- truth: equivalent
-- expect frontend: refuse:parameter-misaligned
-- expect fuzz: not-comparable
-- expect qed: no-plan
-- expect sqleq-solver: no-plan
-- expect sqlsolver-jvm: no-plan
-- expect lean: no-witness
-- binding: gather
-- origin: a proof that may be vacuous is not credited: an upsert of one key twice always fails
-- argument: the second row always collides with the first, which the same statement inserted, so
--   both sides raise on every run

-- The same tuple twice under an upsert on the key: the second row collides with the first, which
-- this statement inserted, and Postgres refuses to update it ("cannot affect row a second time").
-- The pair is equivalent under the gather rule, but only because both sides always fail.
CREATE TABLE counters (name text PRIMARY KEY, n int NOT NULL);
INSERT INTO counters (name, n) VALUES ($1, $2), ($1, $2)
  ON CONFLICT (name) DO UPDATE SET n = counters.n + EXCLUDED.n RETURNING name, n;
INSERT INTO counters (name, n) SELECT * FROM unnest($1::text[], $2::int[])
  ON CONFLICT (name) DO UPDATE SET n = counters.n + EXCLUDED.n RETURNING name, n;
