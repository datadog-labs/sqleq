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
-- origin: issue #64: sqleq-fuzz froze now() at 00:00:00 and localtime at 12:00:00
-- argument: localtime is the time of day of the transaction's start timestamp, which is now()::time
create table "t" ("id" INTEGER, unique ("id"));
SELECT "id", now()::time AS "x" FROM "t";
SELECT "id", localtime AS "x" FROM "t";
