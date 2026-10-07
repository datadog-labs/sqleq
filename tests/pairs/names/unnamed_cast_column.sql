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
-- origin: issue #88: Postgres names an unaliased CAST(v + 1 AS TEXT) after its type, "text", which the
--   frontend left unnamed, so a bare text missed the derived table and resolved to the enclosing u.text
-- witness: u = {(1, '1')}, t = {(5)}: A yields no rows (s.text is '6'), B yields 1
create table "t" ("v" INTEGER);
create table "u" ("id" INTEGER, "text" TEXT);
SELECT "id" FROM "u" WHERE EXISTS (SELECT 1 FROM (SELECT CAST("v" + 1 AS TEXT) FROM "t") AS "s" WHERE "text" = '1');
SELECT "id" FROM "u" WHERE EXISTS (SELECT 1 FROM (SELECT CAST("v" + 1 AS TEXT) FROM "t") AS "s" WHERE "u"."text" = '1');
