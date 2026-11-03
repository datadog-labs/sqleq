-- Unless explicitly stated otherwise all files in this repository are licensed under the
-- Apache License Version 2.0.
-- This product includes software developed at Datadog (https://www.datadoghq.com/).
-- Copyright 2026-Present Datadog, Inc.

-- truth: equivalent
-- expect frontend: emit
-- expect fuzz: no-counterexample
-- expect qed: proved
-- expect sqleq-solver: proved
-- expect sqlsolver-jvm: proved
-- expect lean: unsupported
-- catalog: inferred-seeded
-- origin: group_key_pinned_by_filter.sql with the keys pinned by parameters, as the corpus writes it, and the pinned key kept in the select list
-- argument: every row the WHERE keeps has org = $1 and proj = $2, so grouping by (org, proj, part) and by part make the same groups, and org and
--   proj are $1 and $2 in each
create table "t" ("id" INTEGER PRIMARY KEY, "org" INTEGER, "proj" INTEGER, "part" INTEGER);
SELECT "org", "proj", "part", COUNT(*) FROM "t" WHERE "org" = $1 AND "proj" = $2 GROUP BY "org", "proj", "part";
SELECT $1, $2, "part", COUNT(*) FROM "t" WHERE "org" = $1 AND "proj" = $2 GROUP BY "part";
