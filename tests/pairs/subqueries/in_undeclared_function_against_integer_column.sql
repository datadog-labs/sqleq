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
-- origin: issue #92 item 4: an IN subquery whose operand is the result of a function nobody declared was refused against an integer column
-- argument: the two queries differ only in their aliases
create table "t" ("id" INTEGER PRIMARY KEY, "a" INTEGER);
create table "u" ("id" INTEGER PRIMARY KEY, "b" INTEGER);
SELECT "t"."id" FROM "t" WHERE abs("t"."a") IN (SELECT "u"."b" FROM "u");
SELECT "x"."id" FROM "t" AS "x" WHERE abs("x"."a") IN (SELECT "y"."b" FROM "u" AS "y");
