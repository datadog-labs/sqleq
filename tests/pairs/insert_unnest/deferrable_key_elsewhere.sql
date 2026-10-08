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
-- expect lean: proved-gather
-- binding: gather
-- origin: #110: a deferrable key on another column is no arbiter, so it changes nothing here

-- ON CONFLICT (id) infers the plain primary key; the deferrable key on a plays no part in it.
CREATE TABLE t (id int PRIMARY KEY, a int UNIQUE DEFERRABLE);
INSERT INTO t (id, a) VALUES ($1, $2), ($3, $4) ON CONFLICT (id) DO NOTHING;
INSERT INTO t (id, a) SELECT * FROM unnest($1::int[], $2::int[]) ON CONFLICT (id) DO NOTHING;
