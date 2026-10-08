-- Unless explicitly stated otherwise all files in this repository are licensed under the
-- Apache License Version 2.0.
-- This product includes software developed at Datadog (https://www.datadoghq.com/).
-- Copyright 2026-Present Datadog, Inc.

-- truth: not-equivalent
-- expect frontend: refuse:unsupported
-- expect fuzz: counterexample
-- expect qed: no-plan
-- expect sqleq-solver: no-plan
-- expect lean: unsupported
-- origin: issue #121: sum over a bigint was typed INTEGER in the IR, though Postgres returns numeric, so its division was read as an integer one
-- witness: t = {(1, 1)}: sum(g) / 2 = 0.5 for group 1, so A returns no row and B returns 1
create table "t" ("id" INTEGER, "g" BIGINT);
SELECT "id" FROM "t" GROUP BY "id" HAVING sum("g") / 2 = 0;
SELECT "id" FROM "t" GROUP BY "id" HAVING sum("g") / 2 < 1 AND sum("g") / 2 > -1;
