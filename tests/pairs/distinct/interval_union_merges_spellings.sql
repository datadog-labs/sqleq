-- Unless explicitly stated otherwise all files in this repository are licensed under the
-- Apache License Version 2.0.
-- This product includes software developed at Datadog (https://www.datadoghq.com/).
-- Copyright 2026-Present Datadog, Inc.

-- truth: not-equivalent
-- expect frontend: emit
-- expect fuzz: no-counterexample
-- expect qed: proved !known-unsound
-- expect sqleq-solver: unsupported
-- expect lean: unsupported
-- origin: issue #86: sqleq-solver deduplicated interval values by identity in UNION, though Postgres deduplicates by = and '1 day' = '24 hours'
-- witness: t = {(1, '1 day'), (2, '24 hours')}: UNION keeps one of the two, so A returns one row; B returns two, '1 day' and '24:00:00'
-- The qed pin is the QED prover deduplicating by identity on every type, the frontend half of issue #86.
create table "t" ("id" INTEGER, "x" INTERVAL);
SELECT DISTINCT CAST("x" AS TEXT) FROM (SELECT "x" FROM "t" UNION SELECT "x" FROM "t") AS "s";
SELECT DISTINCT CAST("x" AS TEXT) FROM "t";
