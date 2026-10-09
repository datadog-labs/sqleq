-- Unless explicitly stated otherwise all files in this repository are licensed under the
-- Apache License Version 2.0.
-- This product includes software developed at Datadog (https://www.datadoghq.com/).
-- Copyright 2026-Present Datadog, Inc.

-- truth: not-equivalent
-- expect frontend: refuse:schema
-- expect fuzz: no-counterexample
-- expect qed: no-plan
-- expect sqleq-solver: no-plan
-- expect lean: unsupported
-- origin: an ALTER TABLE after the CREATE TABLE was not read, so after two RENAME COLUMNs the NOT
--   NULL of the column first named a stayed with the name a
-- witness: t = {(1, NULL)}: the column now named a is the nullable one, so A yields no row and B
--   yields NULL
create table "t" ("a" INTEGER NOT NULL, "b" INTEGER);
alter table "t" rename column "a" to "c";
alter table "t" rename column "b" to "a";
SELECT "a" FROM "t" WHERE "a" IS NOT NULL;
SELECT "a" FROM "t";
