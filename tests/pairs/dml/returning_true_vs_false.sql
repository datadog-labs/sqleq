-- Unless explicitly stated otherwise all files in this repository are licensed under the
-- Apache License Version 2.0.
-- This product includes software developed at Datadog (https://www.datadoghq.com/).
-- Copyright 2026-Present Datadog, Inc.

-- truth: not-equivalent
-- expect frontend: emit
-- expect fuzz: counterexample
-- expect qed: no-proof
-- expect sqlsolver-rust: no-proof
-- expect sqlsolver-jvm: no-proof
-- origin: sqleq-fuzz compared only table state after an UPDATE, not what RETURNING returned
-- witness: t = {(1, 0)}: both leave t unchanged, but A returns one row and B none
create table "t" ("id" INTEGER, "a" INTEGER, unique ("id"));
UPDATE "t" SET "a" = "a" WHERE true RETURNING "a";
UPDATE "t" SET "a" = "a" WHERE false RETURNING "a";
