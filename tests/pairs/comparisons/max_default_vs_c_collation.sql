-- Unless explicitly stated otherwise all files in this repository are licensed under the
-- Apache License Version 2.0.
-- This product includes software developed at Datadog (https://www.datadoghq.com/).
-- Copyright 2026-Present Datadog, Inc.

-- truth: not-equivalent
-- expect frontend: emit
-- expect fuzz: no-counterexample
-- expect qed: no-proof
-- expect sqleq-solver: no-proof
-- expect sqlsolver-jvm: no-proof
-- expect lean: unsupported
-- origin: a false proof on 39ed3eb: sqleq-solver read MAX over text and a C-collation > as one uninterpreted order, so it matched the extremum
--   with the anti-join that spells it out under another collation. MAX reads the database's default collation
-- witness: t = {(1, 'a'), (2, 'B')} under en_US.utf8: A is 'B' ('a' < 'B'), B is 'a' (under C, 'B' < 'a'). sqleq-fuzz's cluster runs C, where the two agree
create table "t" ("id" INTEGER PRIMARY KEY, "s" TEXT);
SELECT DISTINCT CAST("m" AS TEXT) FROM (SELECT MAX("s") AS "m" FROM "t") AS "x" WHERE "m" IS NOT NULL;
SELECT DISTINCT CAST("t"."s" AS TEXT) FROM "t" LEFT JOIN "t" AS "u" ON "u"."s" > "t"."s" COLLATE "C" WHERE "u"."id" IS NULL AND "t"."s" IS NOT NULL;
