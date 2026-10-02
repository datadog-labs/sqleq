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
-- origin: SELECT DISTINCT was once lowered as a plain projection
-- witness: t = {(1, 0), (2, 0)}: A returns 0 twice, B once
create table "t" ("id" INTEGER, "v" INTEGER, unique ("id"));
SELECT "v" FROM "t";
SELECT DISTINCT "v" FROM "t";
