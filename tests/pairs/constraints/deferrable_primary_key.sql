-- Unless explicitly stated otherwise all files in this repository are licensed under the
-- Apache License Version 2.0.
-- This product includes software developed at Datadog (https://www.datadoghq.com/).
-- Copyright 2026-Present Datadog, Inc.

-- truth: not-equivalent
-- expect frontend: emit
-- expect fuzz: counterexample
-- expect qed: no-proof
-- expect sqleq-solver: no-proof
-- expect lean: unsupported
-- origin: issue #95: a DEFERRABLE primary key was read as a key, though Postgres checks it only at
--   commit, so DISTINCT over it was taken to be a no-op
-- witness: inside a transaction, after INSERT INTO t VALUES (1, 5), (1, 5) and before commit: A
--   yields 1 row, B yields 2
create table "t" ("id" INTEGER PRIMARY KEY DEFERRABLE INITIALLY DEFERRED, "a" INTEGER);
SELECT DISTINCT "id", "a" FROM "t";
SELECT "id", "a" FROM "t";
