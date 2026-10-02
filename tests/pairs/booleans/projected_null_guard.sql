-- Unless explicitly stated otherwise all files in this repository are licensed under the
-- Apache License Version 2.0.
-- This product includes software developed at Datadog (https://www.datadoghq.com/).
-- Copyright 2026-Present Datadog, Inc.

-- truth: not-equivalent
-- expect frontend: emit
-- expect fuzz: counterexample
-- expect qed: no-proof
-- expect sqlsolver-rust: no-proof
-- expect sqlsolver-jvm: proved !known-unsound
-- expect lean: unsupported
-- catalog: inferred-seeded
-- origin: a boolean in the SELECT list was read two-valued, as if NULL were false
-- witness: t = {(1, NULL)}, $1 = 1: A yields false, B yields NULL

-- In a WHERE clause NULL and false both drop the row, so the same guard there is
-- harmless (booleans/null_guard_in_where.sql); in the SELECT list it is a value.
--
-- The JVM SQLSolver still proves this pair: it reads the projected boolean two-valued.
-- The Rust port keeps NULL apart from false and does not.
create table "t" ("id" INTEGER, "x" INTEGER, unique ("id"));
SELECT "x" IS NOT NULL AND "x" <= 5 FROM "t" WHERE "id" = $1;
SELECT "x" <= 5 FROM "t" WHERE "id" = $1;
