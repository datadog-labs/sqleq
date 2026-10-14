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
-- origin: LIKE ANY is refused (`lower_expr`), as LIKE ALL is (refusals/like_all.sql): ANY over two
--   patterns is not its first pattern alone. No test reached that refusal until this pair
-- witness: t = {(1, 'b')}: 'b' matches the second pattern, so A returns 1; B returns none
create table "t" ("id" INTEGER, "s" TEXT, unique ("id"));
SELECT "id" FROM "t" WHERE "s" LIKE ANY (ARRAY['a%', 'b%']);
SELECT "id" FROM "t" WHERE "s" LIKE 'a%';
