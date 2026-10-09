-- Unless explicitly stated otherwise all files in this repository are licensed under the
-- Apache License Version 2.0.
-- This product includes software developed at Datadog (https://www.datadoghq.com/).
-- Copyright 2026-Present Datadog, Inc.

-- truth: not-equivalent
-- expect frontend: refuse:schema
-- expect fuzz: no-counterexample
-- expect qed: no-plan
-- expect sqleq-solver: no-plan
-- expect lean: unsupported
-- origin: a table created without a column list (LIKE) was dropped from the catalog, so a bare
--   reference to it resolved to the one table of its name in another schema
-- witness: s.t = {}, t = {(1), (1)}: A yields 1 once, B twice
create table "s"."t" ("id" INTEGER PRIMARY KEY);
create table "o" ("id" INTEGER);
create table "t" (like "o");
SELECT DISTINCT "id" FROM "t";
SELECT "id" FROM "t";
