-- Unless explicitly stated otherwise all files in this repository are licensed under the
-- Apache License Version 2.0.
-- This product includes software developed at Datadog (https://www.datadoghq.com/).
-- Copyright 2026-Present Datadog, Inc.

-- truth: not-equivalent
-- expect frontend: emit
-- expect fuzz: no-counterexample
-- expect qed: no-proof
-- expect sqleq-solver: no-proof
-- expect sqlsolver-jvm: no-proof
-- expect lean: unsupported
-- origin: the integer bound tightening of int_bound_discrete.sql is wrong for numeric, whose order sqleq-solver leaves uninterpreted
-- witness: t = {(1, 10.5)}: A keeps the row, B drops it
create table "t" ("id" INTEGER PRIMARY KEY, "d" NUMERIC);
SELECT "id" FROM "t" WHERE "d" > 10;
SELECT "id" FROM "t" WHERE "d" >= 11;
