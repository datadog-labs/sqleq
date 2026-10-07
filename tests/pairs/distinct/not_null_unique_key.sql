-- Unless explicitly stated otherwise all files in this repository are licensed under the
-- Apache License Version 2.0.
-- This product includes software developed at Datadog (https://www.datadoghq.com/).
-- Copyright 2026-Present Datadog, Inc.

-- truth: equivalent
-- expect frontend: emit
-- expect fuzz: no-counterexample
-- expect qed: proved
-- expect sqleq-solver: proved
-- expect lean: unsupported
-- origin: issue #54 (control): with the UNIQUE column NOT NULL the key is kept, and the pair stays
--   proved
-- argument: u is NOT NULL and UNIQUE, so the rows of t already have distinct u
create table "t" ("u" INTEGER NOT NULL, unique ("u"));
SELECT "u" FROM "t";
SELECT DISTINCT "u" FROM "t";
