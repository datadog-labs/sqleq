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
-- origin: date arithmetic was integer arithmetic, which is wrong at infinity (64af5bf)
-- witness: t = {(1, NULL, 'infinity')}: 'infinity' + 1 = 'infinity', so A drops the row and B keeps it
create table "t" ("id" INTEGER, "ts" TIMESTAMP, "d" DATE, unique ("id"));
SELECT "id" FROM "t" WHERE "d" + 1 > "d";
SELECT "id" FROM "t" WHERE "d" IS NOT NULL;
