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
-- origin: issue #92 item 1 (control): the negator of < is >=, not >, so a NOT put through with the wrong operator would prove this pair
-- witness: t = {(1, 1)}: NOT (1 < 1) holds, so A returns 1, and 1 > 1 does not, so B returns nothing
create table "t" ("id" INTEGER PRIMARY KEY, "a" INTEGER);
SELECT "id" FROM "t" WHERE NOT ("a" < 1);
SELECT "id" FROM "t" WHERE "a" > 1;
