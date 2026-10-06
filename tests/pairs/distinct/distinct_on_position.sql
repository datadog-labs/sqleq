-- Unless explicitly stated otherwise all files in this repository are licensed under the
-- Apache License Version 2.0.
-- This product includes software developed at Datadog (https://www.datadoghq.com/).
-- Copyright 2026-Present Datadog, Inc.

-- truth: not-equivalent
-- expect frontend: emit
-- expect fuzz: counterexample
-- expect qed: no-proof
-- expect sqleq-solver: unsupported
-- expect lean: unsupported
-- origin: issue #46: DISTINCT ON (1) is lowered as the constant 1 (one group); Postgres reads
--   it as the first output column, as in ORDER BY 1
-- witness: t = {(1, 0), (2, 0)}: A returns 1 and 2, B returns one row
create table "t" ("a" INTEGER, "b" INTEGER);
SELECT DISTINCT ON (1) "a" FROM "t";
SELECT DISTINCT ON (1 + 0) "a" FROM "t";
