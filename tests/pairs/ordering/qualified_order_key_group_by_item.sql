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
-- origin: issue #92 item 6: a grouped query's qualified ORDER BY key was matched to a select item
--   by how the two are written, so t.a, which is the item a, was refused as not an output column
-- argument: t.a is the grouped column the select item a returns, so both sort the groups by the
--   output column a; the group key is unique per group, so the LIMIT keeps the same groups
create table "t" ("id" INTEGER PRIMARY KEY, "a" INTEGER);
SELECT "a", count(*) AS "n" FROM "t" GROUP BY "a" ORDER BY "t"."a" LIMIT 3;
SELECT "a", count(*) AS "n" FROM "t" GROUP BY "a" ORDER BY "a" LIMIT 3;
