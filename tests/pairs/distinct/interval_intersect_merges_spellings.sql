-- Unless explicitly stated otherwise all files in this repository are licensed under the
-- Apache License Version 2.0.
-- This product includes software developed at Datadog (https://www.datadoghq.com/).
-- Copyright 2026-Present Datadog, Inc.

-- truth: not-equivalent
-- expect frontend: emit
-- expect fuzz: not-comparable
-- expect qed: proved !known-unsound
-- expect sqleq-solver: unsupported
-- expect lean: unsupported
-- origin: issue #86: sqleq-solver deduplicated interval values by identity in INTERSECT, though Postgres deduplicates by = and '1 day' = '24 hours'
-- witness: t = {(1, '1 day'), (2, '24 hours')}: INTERSECT keeps one of the two, so A returns one row; B returns two, 1 and 0
-- The qed pin is the QED prover deduplicating by identity on every type, the frontend half of issue #86.
create table "t" ("id" INTEGER, "x" INTERVAL);
SELECT DISTINCT date_part('day', "x") FROM (SELECT "x" FROM "t" INTERSECT SELECT "x" FROM "t") AS "s";
SELECT DISTINCT date_part('day', "x") FROM "t";
