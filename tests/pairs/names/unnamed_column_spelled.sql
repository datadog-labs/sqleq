-- Unless explicitly stated otherwise all files in this repository are licensed under the
-- Apache License Version 2.0.
-- This product includes software developed at Datadog (https://www.datadoghq.com/).
-- Copyright 2026-Present Datadog, Inc.

-- truth: not-equivalent
-- expect frontend: refuse:unsupported
-- expect fuzz: error
-- expect qed: no-plan
-- expect sqleq-solver: no-plan
-- expect lean: unsupported
-- origin: issue #88: the frontend's placeholder name for an unnamed column, $col0, could be spelled as
--   a quoted identifier and read that column
-- witness: t = {}: Postgres rejects A (column "$col0" does not exist) and B returns no rows.
create table "t" ("id" INTEGER, "x" INTEGER);
SELECT "$col0" FROM (SELECT CASE WHEN "x" > 0 THEN 1 ELSE 0 END FROM "t") AS "s";
SELECT CASE WHEN "x" > 0 THEN 1 ELSE 0 END FROM "t";
