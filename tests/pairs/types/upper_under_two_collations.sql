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
-- origin: issue #87: upper() reads its argument's collation, C for c and the database's default for a constant, and the column's COLLATE was dropped, so c = 'é' let one be substituted for the other
-- witness: in a database created with LC_COLLATE and LC_CTYPE 'en_US.utf8', t = {(1, 'é')}: upper under C leaves 'é' and under en_US.utf8 gives 'É', so A returns 'é' and B returns 'É'
create table "t" ("id" INTEGER, "c" TEXT COLLATE "C");
SELECT upper("c") FROM "t" WHERE "c" = 'é';
SELECT upper('é') FROM "t" WHERE "c" = 'é';
