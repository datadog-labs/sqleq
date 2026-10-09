-- Unless explicitly stated otherwise all files in this repository are licensed under the
-- Apache License Version 2.0.
-- This product includes software developed at Datadog (https://www.datadoghq.com/).
-- Copyright 2026-Present Datadog, Inc.

-- truth: not-equivalent
-- expect frontend: emit
-- expect fuzz: no-counterexample
-- expect qed: no-proof
-- expect sqleq-solver: no-proof
-- expect lean: unsupported
-- origin: an ALTER TABLE after the CREATE TABLE was not read, so a NOT NULL a later DROP NOT NULL
--   dropped was still believed
-- witness: t = {(NULL, 1)}: A yields no row, B yields 1
create table "t" ("id" INTEGER NOT NULL, "a" INTEGER);
alter table "t" alter column "id" drop not null;
SELECT "a" FROM "t" WHERE "id" IS NOT NULL;
SELECT "a" FROM "t";
