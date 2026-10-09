-- Unless explicitly stated otherwise all files in this repository are licensed under the
-- Apache License Version 2.0.
-- This product includes software developed at Datadog (https://www.datadoghq.com/).
-- Copyright 2026-Present Datadog, Inc.

-- truth: not-equivalent
-- expect frontend: refuse:unsupported
-- expect fuzz: counterexample
-- expect qed: no-plan
-- expect sqleq-solver: no-plan
-- expect lean: unsupported
-- origin: a SERIAL spelled quoted, "serial", which Postgres reads as SERIAL, was not seen to be one,
--   so the INSERT reduction took the column for row-determined
-- witness: t = {}, the sequence fresh: A adds (1, 1) and (2, 2), B adds (1, 2) and (2, 1)
create table "t" ("id" "serial", "a" INTEGER);
INSERT INTO t (a) VALUES (1), (2);
INSERT INTO t (a) VALUES (2), (1);
