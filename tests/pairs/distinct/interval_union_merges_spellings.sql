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
-- origin: issue #86: sqleq-solver deduplicated interval values by identity in UNION, though Postgres deduplicates by = and '1 day' = '24 hours'
-- witness: t = {(1, '1 day'), (2, '24 hours')}: UNION keeps one of the two, so A returns one row; B returns two, '1 day' and '24:00:00'
-- QED proved this pair, deduplicating by identity on every type, until issue #84 refused a cast to text over a
-- value whose = is not identity.
create table "t" ("id" INTEGER, "x" INTERVAL);
SELECT DISTINCT CAST("x" AS TEXT) FROM (SELECT "x" FROM "t" UNION SELECT "x" FROM "t") AS "s";
SELECT DISTINCT CAST("x" AS TEXT) FROM "t";
