-- Unless explicitly stated otherwise all files in this repository are licensed under the
-- Apache License Version 2.0.
-- This product includes software developed at Datadog (https://www.datadoghq.com/).
-- Copyright 2026-Present Datadog, Inc.

-- truth: not-equivalent
-- expect frontend: emit
-- expect fuzz: no-counterexample
-- expect qed: no-proof
-- expect sqleq-solver: no-proof
-- expect lean: unsupported
-- origin: a primary key was read on a table another inherits from, though a scan of it reads the
--   inheriting table's rows, which the key does not bind
-- witness: p = {(1)}, c = {(1, 0)}: a scan of p reads 1 twice, so A yields it once and B twice
create table "p" ("id" INTEGER PRIMARY KEY);
create table "c" ("x" INTEGER) inherits ("p");
SELECT DISTINCT "id" FROM "p";
SELECT "id" FROM "p";
