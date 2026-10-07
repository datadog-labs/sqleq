-- Unless explicitly stated otherwise all files in this repository are licensed under the
-- Apache License Version 2.0.
-- This product includes software developed at Datadog (https://www.datadoghq.com/).
-- Copyright 2026-Present Datadog, Inc.

-- truth: equivalent
-- expect frontend: refuse:unsupported
-- expect fuzz: not-comparable
-- expect qed: no-plan
-- expect sqleq-solver: no-plan
-- expect lean: unsupported
-- origin: issue #89: sqleq-fuzz let DuckDB divide a numeric into a DOUBLE, where Postgres divides to a
--   finite decimal scale, so 2 / 3 * 3 came back as 2
-- argument: n has scale 0, so n / 3 is exact, and times 3 gives n back, exactly when 3 divides n;
--   otherwise any finite rounding r of n / 3 has 3 * r <> n (Postgres gives 2.00000000000000000001 for
--   2 / 3 * 3). Postgres 17 returns id 1 for both on t = {(1, 0), (2, NULL), (NULL, 2), (NULL, NULL)}

create table "t" ("id" INTEGER, "n" NUMERIC(10,0), unique ("id"));
SELECT "id" FROM "t" WHERE "n" / 3 * 3 = "n";
SELECT "id" FROM "t" WHERE "n" % 3 = 0;
