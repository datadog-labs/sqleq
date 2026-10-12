-- Unless explicitly stated otherwise all files in this repository are licensed under the
-- Apache License Version 2.0.
-- This product includes software developed at Datadog (https://www.datadoghq.com/).
-- Copyright 2026-Present Datadog, Inc.

-- truth: equivalent
-- expect frontend: refuse:unsupported
-- expect fuzz: no-counterexample
-- expect qed: no-plan
-- expect sqleq-solver: no-plan
-- expect lean: unsupported
-- origin: issue #92 item 6: a grouped query whose ORDER BY key is grouped but not selected is
--   refused, however the key is spelled; the grouped output is not widened with the key
-- argument: no output column is called a, so both keys are the input column t.a, the one grouped
--   column; both sort the groups by it, and the LIMIT keeps the same groups
create table "t" ("id" INTEGER PRIMARY KEY, "a" INTEGER);
SELECT count(*) AS "n" FROM "t" GROUP BY "a" ORDER BY "t"."a" LIMIT 3;
SELECT count(*) AS "n" FROM "t" GROUP BY "a" ORDER BY "a" LIMIT 3;
