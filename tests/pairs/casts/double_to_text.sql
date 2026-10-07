-- Unless explicitly stated otherwise all files in this repository are licensed under the
-- Apache License Version 2.0.
-- This product includes software developed at Datadog (https://www.datadoghq.com/).
-- Copyright 2026-Present Datadog, Inc.

-- truth: equivalent
-- expect frontend: emit
-- expect fuzz: not-comparable
-- expect qed: no-proof
-- expect sqleq-solver: no-proof
-- expect lean: unsupported
-- origin: issue #89: sqleq-fuzz let DuckDB print a double precision as 2.0, where Postgres prints 2
-- argument: every INTEGER value is exactly a double, and Postgres prints a double below 1e15 in magnitude
--   in its shortest round-trip form without an exponent, which for an integral value is the integer's own
--   digits (2147483647 and -2147483648 print as such); so the cast through double precision does not
--   change the text. Postgres 17 returns the same rows for both on i in {-1000, -2, -1, 0, 1, 2, 1000, NULL}

create table "t" ("id" INTEGER, "i" INTEGER, unique ("id"));
SELECT "id", CAST(CAST("i" AS DOUBLE PRECISION) AS TEXT) AS "s" FROM "t";
SELECT "id", CAST("i" AS TEXT) AS "s" FROM "t";
