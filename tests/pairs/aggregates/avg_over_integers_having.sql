-- Unless explicitly stated otherwise all files in this repository are licensed under the
-- Apache License Version 2.0.
-- This product includes software developed at Datadog (https://www.datadoghq.com/).
-- Copyright 2026-Present Datadog, Inc.

-- truth: not-equivalent
-- expect frontend: emit
-- expect fuzz: counterexample
-- expect qed: no-proof
-- expect sqleq-solver: no-proof
-- expect lean: unsupported
-- origin: issue #106: AVG over an integer column was typed INTEGER in the IR, though Postgres returns numeric
-- witness: t = {(1, 0), (1, 1)}: avg(a) = 0.5 for group 1, so A returns no row and B returns 1
create table "t" ("id" INTEGER, "a" INTEGER);
SELECT "id" FROM "t" GROUP BY "id" HAVING AVG("a") = 0;
SELECT "id" FROM "t" GROUP BY "id" HAVING AVG("a") < 1 AND AVG("a") > -1;
