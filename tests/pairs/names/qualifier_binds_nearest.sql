-- Unless explicitly stated otherwise all files in this repository are licensed under the
-- Apache License Version 2.0.
-- This product includes software developed at Datadog (https://www.datadoghq.com/).
-- Copyright 2026-Present Datadog, Inc.

-- truth: not-equivalent
-- expect frontend: refuse:schema
-- expect fuzz: error
-- expect qed: no-plan
-- expect sqleq-solver: no-plan
-- expect lean: unsupported
-- origin: issue #88: a qualified s.x that the nearest s does not have resolved to an enclosing binding
--   also named s, where Postgres stops at the nearest one
-- witness: t = {(1, 1), (2, 2)}: Postgres rejects A (column s.x does not exist) and B returns 1.
create table "t" ("id" INTEGER, "x" INTEGER);
SELECT "s"."id" FROM "t" AS "s" WHERE EXISTS (SELECT 1 FROM (SELECT "x" + 0 FROM "t") AS "s" WHERE "s"."x" = 1);
SELECT "o"."id" FROM "t" AS "o" WHERE EXISTS (SELECT 1 FROM (SELECT "x" + 0 FROM "t") AS "s" WHERE "o"."x" = 1);
