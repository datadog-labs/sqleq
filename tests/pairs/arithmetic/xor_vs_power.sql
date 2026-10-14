-- Unless explicitly stated otherwise all files in this repository are licensed under the
-- Apache License Version 2.0.
-- This product includes software developed at Datadog (https://www.datadoghq.com/).
-- Copyright 2026-Present Datadog, Inc.

-- truth: not-equivalent
-- expect frontend: refuse:unsupported
-- expect fuzz: counterexample
-- expect qed: no-plan
-- expect sqleq-solver: no-plan
-- expect lean: unsupported
-- origin: a binary operator the frontend does not model is refused (`binop`). In Postgres `#` is
--   XOR and `^` is exponentiation; other dialects spell XOR `^`, and a lowering that read it so
--   would make the two sides one query. No test reached that refusal until this pair
-- witness: t = {(1, 2, 3)}: 2 # 3 is 1 and 2 ^ 3 is 8, so A returns 1, B returns 8
create table "t" ("id" INTEGER, "a" INTEGER, "b" INTEGER, unique ("id"));
SELECT "a" # "b" FROM "t";
SELECT "a" ^ "b" FROM "t";
