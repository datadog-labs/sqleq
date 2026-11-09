-- Unless explicitly stated otherwise all files in this repository are licensed under the
-- Apache License Version 2.0.
-- This product includes software developed at Datadog (https://www.datadoghq.com/).
-- Copyright 2026-Present Datadog, Inc.

-- truth: not-equivalent
-- expect frontend: emit
-- expect fuzz: error
-- expect qed: no-proof
-- expect sqleq-solver: no-proof
-- expect cvc5: model
-- expect lean: unsupported
-- origin: issue #203: the DDL reader found a CREATE INDEX's table by its lower-cased name, so a
--   unique index on "T" was read as a key of t
-- witness: "T" ("a" INTEGER NOT NULL, "b" INTEGER) exists, so the index is on it; t = {(1, 1), (1, 2)}:
--   A returns 1 once, B twice
create table "t" ("a" INTEGER NOT NULL, "b" INTEGER);
create unique index "ix" on "T" ("a");
SELECT DISTINCT "a" FROM "t";
SELECT "a" FROM "t";
