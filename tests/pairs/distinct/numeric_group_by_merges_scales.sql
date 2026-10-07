-- Unless explicitly stated otherwise all files in this repository are licensed under the
-- Apache License Version 2.0.
-- This product includes software developed at Datadog (https://www.datadoghq.com/).
-- Copyright 2026-Present Datadog, Inc.

-- truth: not-equivalent
-- expect frontend: emit
-- expect fuzz: error
-- expect qed: proved !known-unsound
-- expect sqleq-solver: unsupported
-- expect lean: unsupported
-- origin: issue #86: sqleq-solver grouped numeric values by identity in GROUP BY, though Postgres groups by = and 2.0 = 2.00
-- witness: t = {(1, 2.0), (2, 2.00)}: GROUP BY makes one group, so A returns one row (scale 1 or 2); B returns two, 1 and 2
-- The qed pin is the QED prover deduplicating by identity on every type, the frontend half of issue #86.
create table "t" ("id" INTEGER, "x" NUMERIC);
SELECT DISTINCT scale("x") FROM (SELECT "x" FROM "t" GROUP BY "x") AS "s";
SELECT DISTINCT scale("x") FROM "t";
