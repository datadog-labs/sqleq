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
-- origin: issue #58: a string literal compared with a BOOLEAN column cast the column to text, where Postgres reads the literal as a boolean
-- witness: t = {(true)}: A yields no rows ('t' and 'true' both read as true), B yields (true)
create table "t" ("p" BOOLEAN);
SELECT "p" FROM "t" WHERE "p" = 't' AND NOT "p" = 'true';
SELECT "p" FROM "t" WHERE "p" = 't';
