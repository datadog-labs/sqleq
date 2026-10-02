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
-- origin: in an inferred catalog the length on a cast over a literal was dropped, so the cast
--   read as the identity (5bbe69e)
-- witness: t = {(1, 'ab')}: an explicit cast truncates, so A compares 'ab' = 'ab' and keeps the
--   row, B compares 'ab' = 'abc' and drops it
create table "t" ("id" INTEGER, "k" VARCHAR, unique ("id"));
SELECT "id" FROM "t" WHERE "k" = 'abc'::varchar(2);
SELECT "id" FROM "t" WHERE "k" = 'abc'::varchar(3);
