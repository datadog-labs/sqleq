-- Unless explicitly stated otherwise all files in this repository are licensed under the
-- Apache License Version 2.0.
-- This product includes software developed at Datadog (https://www.datadoghq.com/).
-- Copyright 2026-Present Datadog, Inc.

-- truth: equivalent
-- expect frontend: refuse:parameter-misaligned
-- expect fuzz: error
-- expect qed: no-plan
-- expect sqlsolver-rust: no-plan
-- expect sqlsolver-jvm: no-plan
-- expect lean: no-witness
-- binding: gather
-- origin: a proof that may be vacuous is not credited: no constraint backs the arbiter
-- argument: both sides raise before reading a row, because no unique or exclusion constraint
--   matches ON CONFLICT (name)

-- ON CONFLICT (name) with no unique constraint on name: Postgres raises before reading a row.
CREATE TABLE tags (id serial PRIMARY KEY, name text, n int);
INSERT INTO tags (name, n) VALUES ($1, $2), ($3, $4) ON CONFLICT (name) DO NOTHING;
INSERT INTO tags (name, n) SELECT * FROM unnest($1::text[], $2::int[]) ON CONFLICT (name) DO NOTHING;
