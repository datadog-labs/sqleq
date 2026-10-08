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
-- origin: #110: a witness is a run that commits on its own, so a deferred key still counts
-- argument: both sides insert the same tuple twice under the key, which no run can commit; inside a
--   transaction the statement passes and COMMIT fails, on both sides alike

-- Checked at COMMIT, not at the statement; the witness model and the replay both count it.
CREATE TABLE t (id int PRIMARY KEY DEFERRABLE INITIALLY DEFERRED, a int);
INSERT INTO t (id, a) VALUES ($1, $2), ($1, $2);
INSERT INTO t (id, a) SELECT * FROM unnest($1::int[], $2::int[]);
