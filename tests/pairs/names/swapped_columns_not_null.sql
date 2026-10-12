-- Unless explicitly stated otherwise all files in this repository are licensed under the
-- Apache License Version 2.0.
-- This product includes software developed at Datadog (https://www.datadoghq.com/).
-- Copyright 2026-Present Datadog, Inc.

-- truth: equivalent
-- expect frontend: refuse:schema
-- expect fuzz: no-counterexample
-- expect qed: no-plan
-- expect sqleq-solver: no-plan
-- expect lean: unsupported
-- origin: issue #143: sqleq-fuzz drew a table's rows under the names and NOT NULLs its CREATE
--   TABLE gave, so after three RENAME COLUMNs swapped a and b, the DuckDB engine drew NULLs for b,
--   the column now NOT NULL, and refuted this pair
-- argument: after the swap b is the column declared a INTEGER NOT NULL, so no row has a NULL b
create table "t" ("a" INTEGER NOT NULL, "b" INTEGER);
alter table "t" rename column "a" to "c";
alter table "t" rename column "b" to "a";
alter table "t" rename column "c" to "b";
SELECT "b" FROM "t" WHERE "b" IS NOT NULL;
SELECT "b" FROM "t";
