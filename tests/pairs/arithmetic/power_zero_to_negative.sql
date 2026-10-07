-- Unless explicitly stated otherwise all files in this repository are licensed under the
-- Apache License Version 2.0.
-- This product includes software developed at Datadog (https://www.datadoghq.com/).
-- Copyright 2026-Present Datadog, Inc.

-- truth: equivalent
-- expect frontend: emit
-- expect fuzz: no-counterexample
-- expect qed: no-proof
-- expect sqleq-solver: no-proof
-- expect lean: unsupported
-- origin: issue #89: sqleq-fuzz let DuckDB answer inf for power(0, -1), where Postgres raises
-- argument: for a <> 0, -power(-a, -1) = -(1 / -a) = 1 / a = power(a, -1); for a = 0 Postgres raises
--   'zero raised to a negative power is undefined' on both sides, so no database with a = 0 is compared
--   (docs/SOUNDNESS.md, a query that raises an error); a NULL gives NULL on both

create table "t" ("id" INTEGER, "a" INTEGER, unique ("id"));
SELECT "id", power("a", -1) AS "r" FROM "t";
SELECT "id", -power(-"a", -1) AS "r" FROM "t";
