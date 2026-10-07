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
-- origin: issue #62: sqleq-fuzz ran integer `/` as floating-point division (DuckDB's default)
-- argument: Postgres integer division truncates toward zero and `%` takes the dividend's sign, so a - a % 2 is the even integer 2 * (a / 2) and halving it is exact
create table "t" ("id" INTEGER, "a" INTEGER, unique ("id"));
SELECT "id", "a" / 2 AS "h" FROM "t";
SELECT "id", ("a" - "a" % 2) / 2 AS "h" FROM "t";
