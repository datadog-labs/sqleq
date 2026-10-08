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
-- origin: issue #89: sqleq-fuzz let DuckDB answer NaN for power(-1, 0.5), where Postgres raises, and NaN >= 0
--   holds in DuckDB; 0.5 is a numeric, so this is numeric power, which DuckDB computes in a DOUBLE and
--   sqleq-fuzz now withholds
-- argument: power(a - 1, 0.5) is a non-negative square root for a >= 1, and for a < 1 Postgres raises 'a
--   negative number raised to a non-integer power yields a complex result', so wherever A runs without
--   an error it selects exactly the rows with a >= 1

create table "t" ("id" INTEGER, "a" INTEGER, unique ("id"));
SELECT "id" FROM "t" WHERE power("a" - 1, 0.5) >= 0;
SELECT "id" FROM "t" WHERE "a" >= 1;
