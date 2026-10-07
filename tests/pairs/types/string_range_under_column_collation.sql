-- Unless explicitly stated otherwise all files in this repository are licensed under the
-- Apache License Version 2.0.
-- This product includes software developed at Datadog (https://www.datadoghq.com/).
-- Copyright 2026-Present Datadog, Inc.

-- truth: not-equivalent
-- expect frontend: emit
-- expect fuzz: no-counterexample
-- expect qed: no-proof
-- expect sqleq-solver: no-proof
-- expect lean: unsupported
-- origin: issue #87: a string range was decided by code point, but s compares under en_US.utf8, where 'a' < 'b' < 'B'
-- witness: t = {(1, 'b')}: under en_US.utf8 'a' < 'b' and 'b' < 'B', so A returns 1 and B returns nothing
create table "t" ("id" INTEGER, "s" TEXT COLLATE "en_US.utf8");
SELECT "id" FROM "t" WHERE "s" > 'a' AND "s" < 'B';
SELECT "id" FROM "t" WHERE FALSE;
