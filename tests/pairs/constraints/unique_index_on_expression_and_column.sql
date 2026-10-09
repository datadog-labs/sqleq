-- Unless explicitly stated otherwise all files in this repository are licensed under the
-- Apache License Version 2.0.
-- This product includes software developed at Datadog (https://www.datadoghq.com/).
-- Copyright 2026-Present Datadog, Inc.

-- truth: not-equivalent
-- expect frontend: emit
-- expect fuzz: counterexample
-- expect qed: no-proof
-- expect sqleq-solver: no-proof
-- expect lean: unsupported
-- origin: issue #92: a unique index on a column and an expression says nothing about the column
--   alone, so it gives no key, not one over the column
-- witness: t = {(1, 'x'), (1, 'y')}: A yields 1 twice, B once
create table "t" ("a" INTEGER NOT NULL, "b" TEXT NOT NULL);
create unique index "t_a_b" on "t" ("a", lower("b"));
SELECT "a" FROM "t";
SELECT DISTINCT "a" FROM "t";
