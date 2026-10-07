-- Unless explicitly stated otherwise all files in this repository are licensed under the
-- Apache License Version 2.0.
-- This product includes software developed at Datadog (https://www.datadoghq.com/).
-- Copyright 2026-Present Datadog, Inc.

-- truth: equivalent
-- expect frontend: refuse:unsupported
-- expect fuzz: not-comparable
-- expect qed: no-plan
-- expect sqleq-solver: no-plan
-- expect lean: unsupported
-- origin: issue #62: sqleq-fuzz materialized CHAR(n) as VARCHAR, so trailing blanks became significant
-- argument: comparing a character(n) value ignores trailing blanks, and the untyped literal is read as character, so 'a' and 'a  ' compare the same
create table "t" ("id" INTEGER, "c" CHAR(3), unique ("id"));
SELECT "id" FROM "t" WHERE "c" = 'a';
SELECT "id" FROM "t" WHERE "c" = 'a  ';
