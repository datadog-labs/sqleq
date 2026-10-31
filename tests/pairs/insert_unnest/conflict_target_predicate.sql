-- Unless explicitly stated otherwise all files in this repository are licensed under the
-- Apache License Version 2.0.
-- This product includes software developed at Datadog (https://www.datadoghq.com/).
-- Copyright 2026-Present Datadog, Inc.

-- truth: equivalent
-- expect frontend: refuse:parameter-misaligned
-- expect fuzz: no-counterexample-gather
-- expect qed: no-plan
-- expect sqleq-solver: no-plan
-- expect sqlsolver-jvm: no-plan
-- expect cvc5: no-plan
-- expect lean: unsupported
-- binding: gather
-- origin: an ON CONFLICT target sqlparser reads only from apache/datafusion-sqlparser-rs#2627; the
--   Lean schema model reads a unique index as total, so it must refuse a partial-index arbiter
--   rather than drop the predicate
-- argument: both sides name the same arbiter, the partial unique index on a where b > 0; under
--   the gather rule they insert the same rows in the same order, so the same rows conflict and
--   are skipped

CREATE TABLE t (a int, b int);
CREATE UNIQUE INDEX t_a_pos ON t (a) WHERE b > 0;
INSERT INTO t (a, b) VALUES ($1, $2), ($3, $4) ON CONFLICT (a) WHERE b > 0 DO NOTHING;
INSERT INTO t (a, b) SELECT * FROM unnest($1::int[], $2::int[]) ON CONFLICT (a) WHERE b > 0 DO NOTHING;
