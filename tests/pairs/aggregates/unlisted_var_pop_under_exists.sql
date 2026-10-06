-- Unless explicitly stated otherwise all files in this repository are licensed under the
-- Apache License Version 2.0.
-- This product includes software developed at Datadog (https://www.datadoghq.com/).
-- Copyright 2026-Present Datadog, Inc.

-- truth: not-equivalent
-- expect frontend: emit
-- expect fuzz: no-counterexample
-- expect qed: no-proof
-- expect sqleq-solver: unsupported
-- expect lean: unsupported
-- origin: issue #51: var_pop, an aggregate on no list, was lowered as a per-row scalar under EXISTS
-- witness: t = {}, s = {(1)}: A yields 1 (the aggregate returns one row on an empty t), B yields nothing
create table "t" ("a" INTEGER);
create table "s" ("x" INTEGER);
SELECT "x" FROM "s" WHERE EXISTS (SELECT var_pop("a") FROM "t");
SELECT "x" FROM "s" WHERE EXISTS (SELECT "a" FROM "t");
