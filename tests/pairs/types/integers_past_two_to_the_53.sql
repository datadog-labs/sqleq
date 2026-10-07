-- Unless explicitly stated otherwise all files in this repository are licensed under the
-- Apache License Version 2.0.
-- This product includes software developed at Datadog (https://www.datadoghq.com/).
-- Copyright 2026-Present Datadog, Inc.

-- truth: not-equivalent
-- expect frontend: emit
-- expect fuzz: counterexample
-- expect qed: no-proof
-- expect sqleq-solver: no-proof
-- expect lean: unsupported
-- origin: issue #61: sqleq-solver compared constants through f64, which cannot tell 2^53 + 1 from 2^53
-- witness: t = {(1, 2)}: the two integers differ, so A returns nothing and B returns 1
create table "t" ("id" INTEGER, "a" BIGINT);
SELECT "id" FROM "t" WHERE 9007199254740993 = 9007199254740992;
SELECT "id" FROM "t";
