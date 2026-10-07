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
-- origin: issue #64: sqleq-fuzz did not make the columns of a table-level PRIMARY KEY NOT NULL
-- argument: a PRIMARY KEY column is NOT NULL, so the filter keeps every row
create table "t" ("id" INTEGER, "a" INTEGER, PRIMARY KEY ("id"));
SELECT "id", "a" FROM "t";
SELECT "id", "a" FROM "t" WHERE "id" IS NOT NULL;
