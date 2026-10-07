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
-- origin: issue #87: max() orders by its argument's collation, C for c and the database's default for d, and the column's COLLATE was dropped, so c = d made the two maxima one
-- witness: in a database created with LC_COLLATE 'en_US.utf8', t = {(1, 'a', 'a'), (2, 'B', 'B')}: under C max is 'a' and under en_US.utf8 'B', so A returns 'a' and B returns 'B'
create table "t" ("id" INTEGER, "c" TEXT COLLATE "C", "d" TEXT);
SELECT MAX("c") FROM "t" WHERE "c" = "d";
SELECT MAX("d") FROM "t" WHERE "c" = "d";
