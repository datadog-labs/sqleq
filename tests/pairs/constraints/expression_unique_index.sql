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
-- origin: issue #64: sqleq-fuzz dropped a unique index on an expression
-- argument: lower(c) is unique, and equal values of c have equal lower(c), so c is unique and DISTINCT removes nothing
create table "t" ("id" INTEGER NOT NULL, "c" TEXT NOT NULL, unique ("id"));
create unique index "t_c_lower" on "t" (lower("c"));
SELECT "c" FROM "t";
SELECT DISTINCT "c" FROM "t";
