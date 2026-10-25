-- Unless explicitly stated otherwise all files in this repository are licensed under the
-- Apache License Version 2.0.
-- This product includes software developed at Datadog (https://www.datadoghq.com/).
-- Copyright 2026-Present Datadog, Inc.

-- truth: equivalent
-- expect frontend: emit-reflexive
-- expect fuzz: no-counterexample
-- expect qed: proved-literal
-- expect sqleq-solver: proved-literal
-- expect cvc5: proved-literal
-- expect lean: unsupported
-- origin: issue #92 item 4: an IN subquery whose column is of a lower type than its operand was refused, though both sides lower to one plan
-- argument: the two queries differ only in their aliases; Postgres compares r = u.id::numeric in both
create table "t" ("id" INTEGER PRIMARY KEY, "r" NUMERIC);
create table "u" ("id" INTEGER PRIMARY KEY);
SELECT "t"."id" FROM "t" WHERE "t"."r" IN (SELECT "u"."id" FROM "u");
SELECT "x"."id" FROM "t" AS "x" WHERE "x"."r" IN (SELECT "y"."id" FROM "u" AS "y");
