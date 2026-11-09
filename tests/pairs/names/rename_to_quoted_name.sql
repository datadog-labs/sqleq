-- Unless explicitly stated otherwise all files in this repository are licensed under the
-- Apache License Version 2.0.
-- This product includes software developed at Datadog (https://www.datadoghq.com/).
-- Copyright 2026-Present Datadog, Inc.

-- truth: not-equivalent
-- expect frontend: refuse:schema
-- expect fuzz: error
-- expect qed: no-plan
-- expect sqleq-solver: no-plan
-- expect cvc5: no-plan
-- expect lean: unsupported
-- origin: issue #203: the DDL reader lower-cased the new name of ALTER TABLE ... RENAME TO "U", so
--   an unquoted u, which Postgres reads as another table, was read as the renamed table, key and all
-- witness: u = {(1, 1), (1, 2)}, "U" = {}: A returns 1 once, B twice
create table "t" ("a" INTEGER PRIMARY KEY, "b" INTEGER);
alter table "t" rename to "U";
SELECT DISTINCT "a" FROM u;
SELECT "a" FROM u;
