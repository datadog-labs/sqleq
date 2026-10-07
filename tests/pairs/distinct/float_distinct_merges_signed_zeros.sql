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
-- origin: issue #86: sqleq-solver deduplicated double precision values by identity in DISTINCT, though Postgres deduplicates by = and 0 = -0
-- witness: t = {(1, 0), (2, -0)}: the inner DISTINCT keeps one of 0 and -0, so A returns one row ('0' or '-0'); B returns two, '0' and '-0'
-- The qed pin is the QED prover deduplicating by identity on every type, the frontend half of issue #86.
create table "t" ("id" INTEGER, "x" DOUBLE PRECISION);
SELECT DISTINCT CAST("x" AS TEXT) FROM (SELECT DISTINCT "x" FROM "t") AS "s";
SELECT DISTINCT CAST("x" AS TEXT) FROM "t";
