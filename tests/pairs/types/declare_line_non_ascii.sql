-- Unless explicitly stated otherwise all files in this repository are licensed under the
-- Apache License Version 2.0.
-- This product includes software developed at Datadog (https://www.datadoghq.com/).
-- Copyright 2026-Present Datadog, Inc.

-- truth: equivalent
-- expect frontend: emit
-- expect fuzz: no-counterexample
-- expect qed: proved
-- expect sqleq-solver: proved
-- expect lean: unsupported
-- origin: issue #67: a declare line with non-ASCII letters in it panicked the frontend, which sliced the line with offsets
--   taken from its Unicode-lowercased copy (İ grows by a byte when lowercased)
-- argument: the declaration names a function neither query calls, and a = 1 AND a = 1 holds exactly when a = 1 does
declare İİ scalar function ée(int) returns int;
create table "t" ("id" INTEGER, "a" INTEGER, unique ("id"));
SELECT "id" FROM "t" WHERE "a" = 1 AND "a" = 1;
SELECT "id" FROM "t" WHERE "a" = 1;
