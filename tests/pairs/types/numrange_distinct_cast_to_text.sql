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
-- origin: issue #108: DISTINCT over numrange keeps one of two equal ranges, which was read as identity
-- witness: t = {(1, '[1.0,2.0)'), (2, '[1.00,2.00)')}: A yields one row, B yields [1.0,2.0) and [1.00,2.00)
create table "t" ("id" INTEGER, "a" numrange);
SELECT DISTINCT CAST("a" AS TEXT) FROM (SELECT DISTINCT "a" FROM "t") AS "s";
SELECT DISTINCT CAST("a" AS TEXT) FROM "t";
