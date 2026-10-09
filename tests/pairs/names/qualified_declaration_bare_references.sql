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
-- origin: a table declared only under a schema, read by its bare name on both sides, as a dump and its queries spell it; resolved to the one declared table of that name
-- argument: both queries read the one table named t and keep the rows with a > 1
create table "s"."t" ("id" INTEGER PRIMARY KEY, "a" INTEGER);
SELECT "id" FROM "t" WHERE "a" > 1;
SELECT "id" FROM "t" WHERE 1 < "a";
