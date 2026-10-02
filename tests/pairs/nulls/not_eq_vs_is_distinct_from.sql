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
-- expect lean: unsupported
-- origin: three-valued logic: NOT (a = 1) is NULL where a IS DISTINCT FROM 1 is true
-- witness: t = {(1, NULL, NULL)}: A drops the row, B keeps it
create table "t" ("id" INTEGER, "a" INTEGER, "b" INTEGER, unique ("id"));
SELECT "id" FROM "t" WHERE NOT ("a" = 1);
SELECT "id" FROM "t" WHERE "a" IS DISTINCT FROM 1;
