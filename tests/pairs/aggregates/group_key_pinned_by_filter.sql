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
-- origin: a GROUP BY key that the WHERE holds to one value is dropped; sqleq-solver lifts the pinned key's equality out of the group's presence
--   squash, so the projected-away key column is eliminated
-- argument: every row the WHERE keeps has org = 1, so grouping by (org, part) and by part make the same groups, with the same counts
create table "t" ("id" INTEGER PRIMARY KEY, "org" INTEGER, "part" INTEGER, "v" INTEGER);
SELECT "part", COUNT(*) FROM "t" WHERE "org" = 1 GROUP BY "org", "part";
SELECT "part", COUNT(*) FROM "t" WHERE "org" = 1 GROUP BY "part";
