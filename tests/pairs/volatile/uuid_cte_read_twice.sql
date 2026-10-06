-- Unless explicitly stated otherwise all files in this repository are licensed under the
-- Apache License Version 2.0.
-- This product includes software developed at Datadog (https://www.datadoghq.com/).
-- Copyright 2026-Present Datadog, Inc.

-- truth: not-equivalent
-- expect frontend: refuse:unsupported
-- expect fuzz: nondet-skip
-- expect qed: no-plan
-- expect sqleq-solver: no-plan
-- expect lean: unsupported
-- origin: issue #49: the reflexivity check inlined an id CTE read twice: A returns one generated
--   uuid twice, B two different ones
-- witness: any database: A returns two equal uuids, B two distinct uuids
create table "t" ("id" INTEGER);
WITH "k" AS (SELECT gen_random_uuid() AS "id") SELECT "id" FROM "k" UNION ALL SELECT "id" FROM "k";
SELECT "id" FROM (SELECT gen_random_uuid() AS "id") AS "k" UNION ALL SELECT "id" FROM (SELECT gen_random_uuid() AS "id") AS "k";
