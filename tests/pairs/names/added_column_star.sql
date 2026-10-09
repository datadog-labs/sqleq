-- Unless explicitly stated otherwise all files in this repository are licensed under the
-- Apache License Version 2.0.
-- This product includes software developed at Datadog (https://www.datadoghq.com/).
-- Copyright 2026-Present Datadog, Inc.

-- truth: not-equivalent
-- expect frontend: refuse:schema
-- expect fuzz: counterexample
-- expect qed: no-plan
-- expect sqleq-solver: no-plan
-- expect lean: unsupported
-- origin: an ALTER TABLE after the CREATE TABLE was not read, so * read the columns before an ADD
--   COLUMN
-- witness: t = {(1, 2)}: A yields (1, 2), B yields (1)
create table "t" ("a" INTEGER);
alter table "t" add column "b" INTEGER;
SELECT * FROM "t";
SELECT "a" FROM "t";
