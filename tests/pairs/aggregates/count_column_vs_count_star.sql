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
-- expect lean: unsupported
-- origin: COUNT(x) must not be read as COUNT(*): one ignores NULLs, the other counts rows
-- witness: t = {(1, NULL)}: A returns 0, B returns 1
create table "t" ("id" INTEGER, "x" INTEGER, unique ("id"));
SELECT count("x") FROM "t";
SELECT count(*) FROM "t";
