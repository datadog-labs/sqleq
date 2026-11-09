-- Unless explicitly stated otherwise all files in this repository are licensed under the
-- Apache License Version 2.0.
-- This product includes software developed at Datadog (https://www.datadoghq.com/).
-- Copyright 2026-Present Datadog, Inc.

-- truth: equivalent
-- expect frontend: refuse:unsupported
-- expect fuzz: no-counterexample
-- expect qed: no-plan
-- expect sqleq-solver: no-plan
-- expect cvc5: no-plan
-- expect lean: unsupported
-- origin: issue #143: sqleq-fuzz could not read a CREATE TABLE ... (LIKE ...) the parser rejects,
--   and found no table t; it now copies the named table's columns, and its keys where the LIKE
--   includes its indexes
-- argument: LIKE ... INCLUDING INDEXES copies o's primary key, so id is a key of t and DISTINCT
--   removes no row
create table "o" ("id" INTEGER PRIMARY KEY, "x" INTEGER);
create table "t" (like "o" including indexes);
SELECT DISTINCT "id" FROM "t";
SELECT "id" FROM "t";
