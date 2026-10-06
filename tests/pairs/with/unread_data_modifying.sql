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
-- origin: issue #50: a data-modifying WITH binding the query never reads was dropped with its
--   effect, so the two statements lowered to one query
-- witness: t = {(7)}, u = {(9)}: both return 7, but A leaves u empty and B leaves u = {(9)}
create table "t" ("a" INTEGER);
create table "u" ("a" INTEGER);
WITH "d" AS (DELETE FROM "u" RETURNING "a") SELECT "a" FROM "t";
SELECT "a" FROM "t";
