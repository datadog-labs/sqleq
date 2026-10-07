-- Unless explicitly stated otherwise all files in this repository are licensed under the
-- Apache License Version 2.0.
-- This product includes software developed at Datadog (https://www.datadoghq.com/).
-- Copyright 2026-Present Datadog, Inc.

-- truth: equivalent
-- expect frontend: emit
-- expect fuzz: no-counterexample
-- expect qed: no-proof
-- expect sqleq-solver: no-proof
-- expect lean: unsupported
-- origin: issue #89: sqleq-fuzz ran a LIKE with no ESCAPE clause on DuckDB, whose LIKE has no escape
--   character; Postgres escapes with a backslash
-- argument: in Postgres a backslash in a LIKE pattern escapes the next character, so with
--   standard_conforming_strings on the pattern '\a' matches exactly the string 'a'. Postgres 17 returns
--   id 1 for both on t = {(1, 'a'), (2, NULL), (NULL, 'c'), (NULL, NULL)}

create table "t" ("id" INTEGER, "s" TEXT, unique ("id"));
SELECT "id" FROM "t" WHERE "s" LIKE '\a';
SELECT "id" FROM "t" WHERE "s" = 'a';
