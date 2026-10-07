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
-- origin: issue #64: `~` reached DuckDB, where it is a full match; in Postgres it matches anywhere in the string
-- argument: in Postgres x ~ 'a' is true when x contains an a, which is x LIKE '%a%'
create table "t" ("id" INTEGER, "c" TEXT, unique ("id"));
SELECT "id" FROM "t" WHERE "c" || 'x' ~ 'a';
SELECT "id" FROM "t" WHERE "c" || 'x' LIKE '%a%';
