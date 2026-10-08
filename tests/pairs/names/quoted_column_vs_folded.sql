-- Unless explicitly stated otherwise all files in this repository are licensed under the
-- Apache License Version 2.0.
-- This product includes software developed at Datadog (https://www.datadoghq.com/).
-- Copyright 2026-Present Datadog, Inc.

-- truth: not-equivalent
-- expect frontend: emit
-- expect fuzz: counterexample
-- expect qed: no-proof
-- expect sqleq-solver: no-proof
-- expect lean: unsupported
-- origin: issue #57: the catalog folded a quoted column "A" to a, so A, which folds to t's a, read m."A"
-- witness: m = {(1, 10)}, t = {(1, 1)}: A returns 10, B returns 1
create table "t" ("id" INTEGER, "a" INTEGER);
create table "m" ("id" INTEGER, "A" INTEGER);
SELECT "A" FROM "m", "t";
SELECT A FROM "m", "t";
