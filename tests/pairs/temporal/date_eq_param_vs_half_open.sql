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
-- catalog: inferred-seeded
-- origin: date arithmetic was integer arithmetic, which is wrong at infinity (64af5bf)
-- witness: t = {(1, NULL, 'infinity')}, $1 = 'infinity': A keeps the row; in B 'infinity' + 1 is 'infinity', so `d < $1 + 1` is false and B drops it
create table "t" ("id" INTEGER, "ts" TIMESTAMP, "d" DATE, unique ("id"));
SELECT "id" FROM "t" WHERE "d" = $1;
SELECT "id" FROM "t" WHERE "d" >= $1 AND "d" < $1 + 1;
