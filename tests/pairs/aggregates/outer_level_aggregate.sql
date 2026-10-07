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
-- origin: issue #51: an aggregate over outer columns only belongs to the outer query, and was
--   lowered as the subquery's
-- witness: t = {(1), (2)}, u = {(1)}: A yields 1 (count(t.a) belongs to the outer query, which
--   returns one row), B yields 2
create table "t" ("a" INTEGER);
create table "u" ("k" INTEGER, unique ("k"));
SELECT count(*) FROM (SELECT (SELECT count("t"."a") FROM "u" WHERE "u"."k" = 1) AS "c" FROM "t") AS "q";
SELECT count(*) FROM "t";
