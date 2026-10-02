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
-- catalog: inferred-seeded
-- origin: the length on a cast over a parameter was dropped (4df30d8)
-- witness: t = {(1, 'ab')}, $1 = 'abc': A compares 'ab' = 'ab' and keeps the row, B compares 'ab' = 'abc' and drops it
create table "t" ("id" INTEGER, "k" VARCHAR, unique ("id"));
SELECT "id" FROM "t" WHERE "k" = $1::varchar(2);
SELECT "id" FROM "t" WHERE "k" = $1::text;
