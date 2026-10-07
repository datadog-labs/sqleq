-- Unless explicitly stated otherwise all files in this repository are licensed under the
-- Apache License Version 2.0.
-- This product includes software developed at Datadog (https://www.datadoghq.com/).
-- Copyright 2026-Present Datadog, Inc.

-- truth: equivalent
-- expect frontend: refuse:unsupported
-- expect fuzz: nondet-skip
-- expect qed: no-plan
-- expect sqleq-solver: no-plan
-- expect lean: unsupported
-- origin: issue #89: sqleq-fuzz compared how DuckDB numbered rows tied in a window ORDER BY as a bag,
--   though Postgres numbers them by physical order
-- argument: B's inner ORDER BY does not constrain the window's sort, so both sides number the rows tied
--   on g in an arbitrary order, read as the same choice (docs/SOUNDNESS.md, "A row slice is taken as
--   deterministic"). On t = {(1, 0, 2), (NULL, NULL, 2), (NULL, 0, 1), (0, 0, 2)} Postgres 17 gives
--   id 1 the number 1 in A and 2 in B, and 3 in A once the same rows are inserted in reverse order

create table "t" ("id" INTEGER, "g" INTEGER, "v" INTEGER, unique ("id"));
SELECT "id", row_number() OVER (ORDER BY "g") AS "rn" FROM "t";
SELECT "id", row_number() OVER (ORDER BY "g") AS "rn" FROM (SELECT * FROM "t" ORDER BY "id" DESC) AS "s";
