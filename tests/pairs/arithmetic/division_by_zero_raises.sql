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
-- origin: issue #62: sqleq-fuzz let DuckDB answer inf / -inf for a division by zero where Postgres raises
-- argument: for a <> 0 both sides compute 1.0 / a; for a = 0 Postgres raises division by zero on both sides; for a NULL both are NULL
create table "t" ("id" INTEGER, "a" INTEGER, unique ("id"));
SELECT "id", 1.0 / "a" AS "x" FROM "t";
SELECT "id", -1.0 / -"a" AS "x" FROM "t";
