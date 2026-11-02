-- Unless explicitly stated otherwise all files in this repository are licensed under the
-- Apache License Version 2.0.
-- This product includes software developed at Datadog (https://www.datadoghq.com/).
-- Copyright 2026-Present Datadog, Inc.

-- truth: not-equivalent
-- expect frontend: emit
-- expect fuzz: error
-- expect qed: no-proof
-- expect sqleq-solver: no-proof
-- expect lean: unsupported
-- origin: issue #203: the DDL reader found an ALTER TABLE's target by its lower-cased name, so a
--   primary key added to "T" was put on t
-- witness: "T" ("a" INTEGER NOT NULL, "b" INTEGER) exists, so the ALTER keys it; t = {(1, 1), (1, 2)}:
--   A returns 1 once, B twice
create table "t" ("a" INTEGER NOT NULL, "b" INTEGER);
alter table "T" add primary key ("a");
SELECT DISTINCT "a" FROM "t";
SELECT "a" FROM "t";
