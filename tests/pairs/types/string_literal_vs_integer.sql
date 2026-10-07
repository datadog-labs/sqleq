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
-- origin: issue #58: a string literal compared with an INTEGER column cast the column to text, where Postgres reads the literal as an integer
-- witness: t = {(1)}: A yields no rows ('1' and '01' both read as the integer 1), B yields (1)
create table "t" ("a" INTEGER);
SELECT "a" FROM "t" WHERE "a" = '1' AND NOT "a" = '01';
SELECT "a" FROM "t" WHERE "a" = '1';
