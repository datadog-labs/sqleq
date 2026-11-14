-- Unless explicitly stated otherwise all files in this repository are licensed under the
-- Apache License Version 2.0.
-- This product includes software developed at Datadog (https://www.datadoghq.com/).
-- Copyright 2026-Present Datadog, Inc.

-- truth: equivalent
-- expect frontend: refuse:unsupported
-- expect fuzz: no-counterexample
-- expect qed: no-plan
-- expect sqleq-solver: no-plan
-- expect cvc5: no-plan
-- expect lean: unsupported
-- origin: issue #89: sqleq-fuzz let DuckDB divide a numeric into a DOUBLE, where Postgres divides to a
--   finite decimal scale, so 2 / 3 * 3 came back as 2. arithmetic/numeric_division_rounds.sql was this pair
--   without the CHECK, which is not equivalent at NaN (issue #234); the CHECK keeps the table to finite values
-- argument: n has scale 0, so n / 3 is exact, and times 3 gives n back, exactly when 3 divides n;
--   otherwise any finite rounding r of n / 3 has 3 * r <> n (Postgres gives 2.00000000000000000001 for
--   2 / 3 * 3). NaN is the one value of numeric(10,0) that is not finite, and the CHECK refuses it

create table "t" ("id" INTEGER, "n" NUMERIC(10,0) CHECK ("n" <> 'NaN'), unique ("id"));
SELECT "id" FROM "t" WHERE "n" / 3 * 3 = "n";
SELECT "id" FROM "t" WHERE "n" % 3 = 0;
