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
-- origin: issue #92: a key declared by CREATE UNIQUE INDEX was not read
-- argument: the unique index makes the NOT NULL id unique, so DISTINCT removes nothing
create table "t" ("id" INTEGER NOT NULL, "a" INTEGER);
create unique index "t_id" on "t" ("id");
SELECT "id" FROM "t";
SELECT DISTINCT "id" FROM "t";
