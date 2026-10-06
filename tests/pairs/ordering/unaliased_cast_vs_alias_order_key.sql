-- Unless explicitly stated otherwise all files in this repository are licensed under the
-- Apache License Version 2.0.
-- This product includes software developed at Datadog (https://www.datadoghq.com/).
-- Copyright 2026-Present Datadog, Inc.

-- truth: not-equivalent
-- expect frontend: emit-reflexive !known-unsound
-- expect fuzz: no-counterexample
-- expect qed: proved-literal !known-unsound
-- expect sqleq-solver: proved-literal !known-unsound
-- expect lean: unsupported
-- origin: issue #46: an unaliased CAST(a AS TEXT) is the output column a in Postgres, so A's bare
--   key a sorts by the text; B aliases the cast w, so its key a is the input column. Lowering
--   resolves the key that way (ordering/unaliased_cast_order_key.sql), but this pair never reaches
--   it: normalize::strip_identical_pagination drops the identical ORDER BY a LIMIT 1 from both
--   sides first, since each projects an expression spelled a, and the rest is one query. The
--   markers stand until that strip compares what the keys resolve to rather than their text.
-- witness: t = {(1, 9, 0), (2, 10, 0)}: A returns ('10', 10), B returns ('9', 9)
create table "t" ("id" INTEGER, "a" INTEGER, "b" INTEGER, unique ("id"));
SELECT CAST("a" AS TEXT), "a" AS "z" FROM "t" ORDER BY "a" LIMIT 1;
SELECT CAST("a" AS TEXT) AS "w", "a" AS "z" FROM "t" ORDER BY "a" LIMIT 1;
