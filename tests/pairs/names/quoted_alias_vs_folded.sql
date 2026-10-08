-- Unless explicitly stated otherwise all files in this repository are licensed under the
-- Apache License Version 2.0.
-- This product includes software developed at Datadog (https://www.datadoghq.com/).
-- Copyright 2026-Present Datadog, Inc.

-- truth: not-equivalent
-- expect frontend: emit
-- expect fuzz: counterexample
-- expect qed: no-proof
-- expect sqleq-solver: no-proof
-- expect lean: unsupported
-- origin: issue #57: table aliases were lower-cased, so "X" and x were one alias and both read the
--   first relation
-- witness: t = {(1, 1)}, u = {(1, 5)}: A reads t and returns 1, B reads u and returns 5
create table "t" ("id" INTEGER, "a" INTEGER);
create table "u" ("id" INTEGER, "a" INTEGER);
SELECT "X"."a" FROM "t" AS "X", "u" AS x;
SELECT x."a" FROM "t" AS "X", "u" AS x;
