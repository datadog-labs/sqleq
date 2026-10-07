-- Unless explicitly stated otherwise all files in this repository are licensed under the
-- Apache License Version 2.0.
-- This product includes software developed at Datadog (https://www.datadoghq.com/).
-- Copyright 2026-Present Datadog, Inc.

-- truth: equivalent
-- expect frontend: emit
-- expect fuzz: no-counterexample
-- expect qed: proved
-- expect sqleq-solver: no-proof
-- expect lean: unsupported
-- origin: issue #64: sqleq-fuzz read UNIQUE NULLS NOT DISTINCT as a plain UNIQUE, which admits many NULLs
-- argument: under NULLS NOT DISTINCT at most one row has a NULL a, so every value of a, NULL included, occurs once and DISTINCT removes nothing
create table "t" ("id" INTEGER, "a" INTEGER, unique nulls not distinct ("a"));
SELECT "a" FROM "t";
SELECT DISTINCT "a" FROM "t";
