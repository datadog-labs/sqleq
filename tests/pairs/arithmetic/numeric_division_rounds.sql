-- Unless explicitly stated otherwise all files in this repository are licensed under the
-- Apache License Version 2.0.
-- This product includes software developed at Datadog (https://www.datadoghq.com/).
-- Copyright 2026-Present Datadog, Inc.

-- truth: not-equivalent
-- expect frontend: refuse:unsupported
-- expect fuzz: counterexample
-- expect qed: no-plan
-- expect sqleq-solver: no-plan
-- expect cvc5: no-plan
-- expect lean: unsupported
-- origin: issue #89: sqleq-fuzz let DuckDB divide a numeric into a DOUBLE, where Postgres divides to a
--   finite decimal scale, so 2 / 3 * 3 came back as 2
--   The pair was pinned equivalent, which it is on every finite n (arithmetic/numeric_division_rounds_on_finite_values.sql).
--   A numeric(10,0) holds NaN too (issue #234), and there the two differ
-- witness: t = {(1, 'NaN')}: NaN / 3 * 3 is NaN and NaN = NaN holds, so A returns 1; NaN % 3 is NaN, which is not
--   0, so B returns no row

create table "t" ("id" INTEGER, "n" NUMERIC(10,0), unique ("id"));
SELECT "id" FROM "t" WHERE "n" / 3 * 3 = "n";
SELECT "id" FROM "t" WHERE "n" % 3 = 0;
