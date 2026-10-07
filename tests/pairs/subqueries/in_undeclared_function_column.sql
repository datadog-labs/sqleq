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
-- origin: issue #67: the QED prover panicked on x IN (SELECT <undeclared function> ...), because the frontend left the
--   call's VARBINARY type against the INTEGER operand; the operand is now cast to the column's type
-- argument: abs(a) is NULL exactly when a is NULL, and a NULL in the IN list only turns false into NULL, which WHERE drops just the same
create table "t" ("k" INTEGER, "x" INTEGER, "a" INTEGER, unique ("k"));
SELECT "k" FROM "t" WHERE "x" IN (SELECT abs("a") FROM "t");
SELECT "k" FROM "t" WHERE "x" IN (SELECT abs("a") FROM "t" WHERE "a" IS NOT NULL);
