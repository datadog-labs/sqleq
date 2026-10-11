-- Unless explicitly stated otherwise all files in this repository are licensed under the
-- Apache License Version 2.0.
-- This product includes software developed at Datadog (https://www.datadoghq.com/).
-- Copyright 2026-Present Datadog, Inc.

-- truth: not-equivalent
-- expect frontend: refuse:parameter-misaligned
-- expect fuzz: counterexample-gather
-- expect qed: no-plan
-- expect sqleq-solver: no-plan
-- expect sqlsolver-jvm: no-plan
-- expect lean: unsupported
-- binding: gather
-- origin: the gather rule in sqleq-fuzz: text elements assigned to a uuid column
-- witness: $1 = '00000000-0000-0000-0000-000000000001', $2 = 'x': A inserts the row; B is
--   rejected, because column id is uuid and the unnest side gives it text

-- The unnest side's elements are text, and Postgres assigns no text to a uuid column.
CREATE TABLE t (id uuid PRIMARY KEY, name text);
INSERT INTO t (id, name) VALUES ($1, $2);
INSERT INTO t (id, name) SELECT * FROM unnest($1::text[], $2::text[]);
