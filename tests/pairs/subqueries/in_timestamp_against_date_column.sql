-- Unless explicitly stated otherwise all files in this repository are licensed under the
-- Apache License Version 2.0.
-- This product includes software developed at Datadog (https://www.datadoghq.com/).
-- Copyright 2026-Present Datadog, Inc.

-- truth: equivalent
-- expect frontend: emit-reflexive
-- expect fuzz: no-counterexample
-- expect qed: proved-literal
-- expect sqleq-solver: proved-literal
-- expect lean: unsupported
-- origin: issue #92 item 4: an IN subquery over a date column, against a timestamp operand, was refused; Postgres converts the column
-- argument: the two queries differ only in their aliases; Postgres compares ts = u.d::timestamp in both
create table "t" ("id" INTEGER PRIMARY KEY, "ts" TIMESTAMP);
create table "u" ("id" INTEGER PRIMARY KEY, "d" DATE);
SELECT "t"."id" FROM "t" WHERE "t"."ts" IN (SELECT "u"."d" FROM "u");
SELECT "x"."id" FROM "t" AS "x" WHERE "x"."ts" IN (SELECT "y"."d" FROM "u" AS "y");
