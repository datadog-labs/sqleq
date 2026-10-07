-- Unless explicitly stated otherwise all files in this repository are licensed under the
-- Apache License Version 2.0.
-- This product includes software developed at Datadog (https://www.datadoghq.com/).
-- Copyright 2026-Present Datadog, Inc.

-- truth: not-equivalent
-- expect frontend: reflexive !known-unsound
-- expect fuzz: counterexample
-- expect qed: no-plan
-- expect sqleq-solver: no-plan
-- expect lean: unsupported
-- origin: issue #64: sqleq-fuzz loaded s1.t and s2.t from one generated table, so two different tables always held the same rows
-- witness: s1.t = {(1)}, s2.t = {(2)}: A yields 1, B yields 2
-- The frontend strips each side's schema before comparing, so it finds the two sides one query
-- (issue #48); that line stays marked until the frontend keeps the schemas apart.
create table "s1"."t" ("a" INTEGER);
create table "s2"."t" ("a" INTEGER);
SELECT "a" FROM "s1"."t";
SELECT "a" FROM "s2"."t";
