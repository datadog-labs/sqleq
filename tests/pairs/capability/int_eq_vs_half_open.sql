-- Unless explicitly stated otherwise all files in this repository are licensed under the
-- Apache License Version 2.0.
-- This product includes software developed at Datadog (https://www.datadoghq.com/).
-- Copyright 2026-Present Datadog, Inc.

-- truth: equivalent
-- expect frontend: emit
-- expect fuzz: no-counterexample
-- expect qed: proved
-- expect sqleq-solver: no-proof
-- expect sqlsolver-jvm: proved
-- expect lean: unsupported
-- origin: integer reasoning across a half-open range, which is sound over integers and not
--   over dates (temporal/date_eq_param_vs_half_open.sql)
-- argument: over integers, x >= 3 AND x < 4 holds exactly when x = 3, and both are NULL when x is
create table "t" ("id" INTEGER, "x" INTEGER, unique ("id"));
SELECT "id" FROM "t" WHERE "x" = 3;
SELECT "id" FROM "t" WHERE "x" >= 3 AND "x" < 3 + 1;
