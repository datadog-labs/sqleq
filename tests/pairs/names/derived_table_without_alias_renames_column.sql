-- Unless explicitly stated otherwise all files in this repository are licensed under the
-- Apache License Version 2.0.
-- This product includes software developed at Datadog (https://www.datadoghq.com/).
-- Copyright 2026-Present Datadog, Inc.

-- truth: not-equivalent
-- expect frontend: emit
-- expect fuzz: counterexample
-- expect qed: no-proof
-- expect sqleq-solver: no-proof
-- expect sqlsolver-jvm: no-proof
-- expect cvc5: model
-- expect lean: unsupported
-- origin: the control beside derived_table_without_alias.sql (#192)
-- witness: t = {(1, 2)}: A returns 2, B returns 1
create table "t" ("a" INTEGER, "b" INTEGER);
SELECT "a" FROM (SELECT "b" AS "a" FROM "t");
SELECT "a" FROM "t";
