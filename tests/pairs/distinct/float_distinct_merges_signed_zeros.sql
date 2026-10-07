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
-- origin: issue #86: sqleq-solver deduplicated double precision values by identity in DISTINCT, though Postgres deduplicates by = and 0 = -0
-- witness: t = {(1, 0), (2, -0)}: the inner DISTINCT keeps one of 0 and -0, so A returns one row ('0' or '-0'); B returns two, '0' and '-0'
-- QED proved this pair, deduplicating by identity on every type, until issue #84 refused a cast to text over a
-- value whose = is not identity.
create table "t" ("id" INTEGER, "x" DOUBLE PRECISION);
SELECT DISTINCT CAST("x" AS TEXT) FROM (SELECT DISTINCT "x" FROM "t") AS "s";
SELECT DISTINCT CAST("x" AS TEXT) FROM "t";
