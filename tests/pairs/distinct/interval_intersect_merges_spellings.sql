-- Unless explicitly stated otherwise all files in this repository are licensed under the
-- Apache License Version 2.0.
-- This product includes software developed at Datadog (https://www.datadoghq.com/).
-- Copyright 2026-Present Datadog, Inc.

-- truth: not-equivalent
-- expect frontend: refuse:unsupported
-- expect fuzz: not-comparable
-- expect qed: no-plan
-- expect sqleq-solver: no-plan
-- expect lean: unsupported
-- origin: issue #86: sqleq-solver deduplicated interval values by identity in INTERSECT, though Postgres deduplicates by = and '1 day' = '24 hours'
-- witness: t = {(1, '1 day'), (2, '24 hours')}: INTERSECT keeps one of the two, so A returns one row; B returns two, 1 and 0
-- QED proved this pair, deduplicating by identity on every type, until issue #84 refused date_part over a
-- value whose = is not identity.
create table "t" ("id" INTEGER, "x" INTERVAL);
SELECT DISTINCT date_part('day', "x") FROM (SELECT "x" FROM "t" INTERSECT SELECT "x" FROM "t") AS "s";
SELECT DISTINCT date_part('day', "x") FROM "t";
