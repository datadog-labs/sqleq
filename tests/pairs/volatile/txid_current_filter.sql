-- Unless explicitly stated otherwise all files in this repository are licensed under the
-- Apache License Version 2.0.
-- This product includes software developed at Datadog (https://www.datadoghq.com/).
-- Copyright 2026-Present Datadog, Inc.

-- truth: equivalent
-- expect frontend: emit
-- expect fuzz: nondet-skip
-- expect qed: proved
-- expect sqleq-solver: proved
-- expect lean: unsupported
-- origin: issue #63: txid_current() was missing from sqleq-fuzz's list of nondeterministic functions
-- argument: the WHERE clause is true on every row of a NOT NULL column, and in one transaction txid_current() is the same value on both sides
create table "t" ("id" INTEGER NOT NULL, unique ("id"));
SELECT "id", txid_current() AS "x" FROM "t";
SELECT "id", txid_current() AS "x" FROM "t" WHERE "id" IS NOT NULL;
