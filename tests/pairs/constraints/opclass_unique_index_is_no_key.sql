-- Unless explicitly stated otherwise all files in this repository are licensed under the
-- Apache License Version 2.0.
-- This product includes software developed at Datadog (https://www.datadoghq.com/).
-- Copyright 2026-Present Datadog, Inc.

-- truth: equivalent
-- expect frontend: emit
-- expect fuzz: no-counterexample
-- expect qed: no-proof
-- expect sqleq-solver: no-proof
-- expect lean: unsupported
-- origin: issue #92: a unique index under an operator class gives no key, since its = need not be the
--   column's; the cost of that rule
-- argument: text_pattern_ops compares by the = of text, so s is unique and DISTINCT removes nothing
create table "t" ("id" INTEGER NOT NULL, "s" TEXT NOT NULL);
create unique index "t_s" on "t" ("s" text_pattern_ops);
SELECT "s" FROM "t";
SELECT DISTINCT "s" FROM "t";
