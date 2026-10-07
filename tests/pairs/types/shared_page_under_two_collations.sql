-- Unless explicitly stated otherwise all files in this repository are licensed under the
-- Apache License Version 2.0.
-- This product includes software developed at Datadog (https://www.datadoghq.com/).
-- Copyright 2026-Present Datadog, Inc.

-- truth: not-equivalent
-- expect frontend: refuse:unsupported
-- expect fuzz: no-counterexample
-- expect qed: no-plan
-- expect sqleq-solver: no-plan
-- expect lean: unsupported
-- origin: issue #87: the ORDER BY ... LIMIT both sides share was stripped, but it sorts by c under C on one side and by d under the database's default on the other
-- witness: in a database created with LC_COLLATE 'en_US.utf8', t = {(1, 'a', 'a'), (2, 'B', 'B')}: under C 'B' sorts first and under en_US.utf8 'a', so A returns 'B' and B returns 'a'
create table "t" ("id" INTEGER, "c" TEXT COLLATE "C", "d" TEXT);
SELECT "c" AS "x" FROM "t" WHERE "c" = "d" ORDER BY "x" LIMIT 1;
SELECT "d" AS "x" FROM "t" WHERE "c" = "d" ORDER BY "x" LIMIT 1;
