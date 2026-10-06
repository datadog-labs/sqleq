-- Unless explicitly stated otherwise all files in this repository are licensed under the
-- Apache License Version 2.0.
-- This product includes software developed at Datadog (https://www.datadoghq.com/).
-- Copyright 2026-Present Datadog, Inc.

-- truth: not-equivalent
-- expect frontend: refuse:unsupported
-- expect fuzz: counterexample
-- expect qed: no-plan
-- expect sqleq-solver: no-plan
-- expect lean: unsupported
-- catalog: inferred
-- origin: issue #50: under an inferred catalog only a top-level WITH was refused; a WITH RECURSIVE
--   nested in a derived table was ignored by lowering, as in the declared mode
-- witness: t = {(7)}: A returns 1, B returns 7
create table "t" ("a" INTEGER);
SELECT "s"."a" FROM (WITH RECURSIVE "t" AS (SELECT 1 AS "a") SELECT "a" FROM "t") AS "s";
SELECT "s"."a" FROM (SELECT "a" FROM "t") AS "s";
