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
-- origin: issue #67: the QED prover panicked on an IN subquery whose column (REAL, from a / 2.0) has another type than the
--   INTEGER operand, because the frontend coerced only temporal mismatches; the operand is now converted to REAL
-- argument: a / 2.0 is NULL exactly when a is NULL, and a NULL in the IN list only turns false into NULL, which WHERE drops just the same
create table "t" ("id" INTEGER, "a" INTEGER, unique ("id"));
SELECT "id" FROM "t" WHERE "id" IN (SELECT "a" / 2.0 FROM "t");
SELECT "id" FROM "t" WHERE "id" IN (SELECT "a" / 2.0 FROM "t" WHERE "a" IS NOT NULL);
