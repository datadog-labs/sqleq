-- Unless explicitly stated otherwise all files in this repository are licensed under the
-- Apache License Version 2.0.
-- This product includes software developed at Datadog (https://www.datadoghq.com/).
-- Copyright 2026-Present Datadog, Inc.

-- truth: not-equivalent
-- expect frontend: emit
-- expect fuzz: counterexample
-- expect qed: no-proof
-- expect sqleq-solver: unsupported
-- expect lean: unsupported
-- origin: issue #51: bit_or, an aggregate on no list, was lowered as a per-row scalar under IN
-- witness: t = {(1), (2)}, s = {(3)}: A yields 3 (bit_or over all of t), B yields nothing (the
--   groups give 1 and 2)
create table "t" ("a" INTEGER);
create table "s" ("x" INTEGER);
SELECT "x" FROM "s" WHERE "x" IN (SELECT CAST(bit_or("a") AS INTEGER) FROM "t");
SELECT "x" FROM "s" WHERE "x" IN (SELECT CAST(bit_or("a") AS INTEGER) FROM "t" GROUP BY "a");
