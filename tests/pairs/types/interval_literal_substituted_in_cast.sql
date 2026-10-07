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
-- origin: issue #84: a filter x = INTERVAL '1 day' let the literal stand in for x inside a cast to text
-- witness: t = {('24 hours')}: A yields '24:00:00', B yields '1 day'
create table "t" ("x" INTERVAL);
SELECT CAST("x" AS TEXT) FROM "t" WHERE "x" = INTERVAL '1 day';
SELECT CAST(INTERVAL '1 day' AS TEXT) FROM "t" WHERE "x" = INTERVAL '1 day';
