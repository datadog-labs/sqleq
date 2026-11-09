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
-- origin: issue #92 item 6: a grouped query's bare ORDER BY key that names no output column was
--   matched to a select item by how the two are written, so a, which is the item t.a AS k, was
--   refused as not an output column
-- argument: no output column is called a, so the key a is the input column t.a, which is the value
--   of the output column k; both sort the groups by k, and the LIMIT keeps the same groups
create table "t" ("id" INTEGER PRIMARY KEY, "a" INTEGER);
SELECT "t"."a" AS "k", count(*) AS "n" FROM "t" GROUP BY "t"."a" ORDER BY "a" LIMIT 3;
SELECT "t"."a" AS "k", count(*) AS "n" FROM "t" GROUP BY "t"."a" ORDER BY "k" LIMIT 3;
