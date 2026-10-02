-- Unless explicitly stated otherwise all files in this repository are licensed under the
-- Apache License Version 2.0.
-- This product includes software developed at Datadog (https://www.datadoghq.com/).
-- Copyright 2026-Present Datadog, Inc.

-- truth: not-equivalent
-- expect frontend: emit
-- expect fuzz: no-counterexample
-- expect qed: no-proof
-- expect sqlsolver-rust: no-proof
-- expect sqlsolver-jvm: no-proof
-- expect lean: unsupported
-- origin: the length on a cast over a column was dropped (4df30d8)
-- witness: t = {(1, 'abc')}: A yields 'ab', B yields 'abc'
create table "t" ("id" INTEGER, "k" VARCHAR, unique ("id"));
SELECT "k"::varchar(2) FROM "t";
SELECT "k"::varchar(3) FROM "t";
