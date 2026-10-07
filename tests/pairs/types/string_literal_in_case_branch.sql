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
-- origin: issue #58: a string literal CASE branch made an integer CASE text, so its comparisons with literals were string comparisons
-- witness: t = {(1, 1, true)}: the CASE is the integer 1, and '1' and '01' both read as 1, so A yields no rows and B yields (1)
create table "t" ("id" INTEGER, "a" INTEGER, "p" BOOLEAN, unique ("id"));
SELECT "a" FROM "t" WHERE CASE WHEN "p" THEN "a" ELSE '7' END = '1' AND NOT CASE WHEN "p" THEN "a" ELSE '7' END = '01';
SELECT "a" FROM "t" WHERE CASE WHEN "p" THEN "a" ELSE '7' END = '1';
