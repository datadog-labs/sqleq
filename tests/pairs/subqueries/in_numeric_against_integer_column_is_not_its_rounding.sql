-- Unless explicitly stated otherwise all files in this repository are licensed under the
-- Apache License Version 2.0.
-- This product includes software developed at Datadog (https://www.datadoghq.com/).
-- Copyright 2026-Present Datadog, Inc.

-- truth: not-equivalent
-- expect frontend: emit
-- expect fuzz: no-counterexample
-- expect qed: no-proof
-- expect sqleq-solver: no-proof
-- expect cvc5: no-proof
-- expect lean: unsupported
-- origin: issue #92 item 4: the guard for the conversion of an IN subquery's column: the column is converted to the operand's type, not the operand to the column's
-- witness: t = {(1, 1.5)}, u = {(2)}: A compares 1.5 = 2 and returns nothing; B compares 2 = 2 and returns 1
create table "t" ("id" INTEGER PRIMARY KEY, "r" NUMERIC);
create table "u" ("id" INTEGER PRIMARY KEY);
SELECT "t"."id" FROM "t" WHERE "t"."r" IN (SELECT "u"."id" FROM "u");
SELECT "t"."id" FROM "t" WHERE CAST("t"."r" AS INTEGER) IN (SELECT "u"."id" FROM "u");
