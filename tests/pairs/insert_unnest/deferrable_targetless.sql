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
-- origin: #110: without a target every unique index is an arbiter, a deferrable one included
-- argument: Postgres never takes a deferrable constraint as an ON CONFLICT arbiter, so both
--   sides raise 55000 before reading a row, on every run

-- The primary key is not deferrable, but the unique key on a is, and it arbitrates too.
CREATE TABLE t (id int PRIMARY KEY, a int UNIQUE DEFERRABLE);
INSERT INTO t (id, a) VALUES ($1, $2), ($3, $4) ON CONFLICT DO NOTHING;
INSERT INTO t (id, a) SELECT * FROM unnest($1::int[], $2::int[]) ON CONFLICT DO NOTHING;
