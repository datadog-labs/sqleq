-- Unless explicitly stated otherwise all files in this repository are licensed under the
-- Apache License Version 2.0.
-- This product includes software developed at Datadog (https://www.datadoghq.com/).
-- Copyright 2026-Present Datadog, Inc.

-- truth: not-equivalent
-- expect frontend: emit
-- expect fuzz: no-counterexample
-- expect qed: no-proof
-- expect sqleq-solver: unsupported
-- expect lean: unsupported
-- origin: issue #46: an unaliased cast had no output name, so the bare key x reached the input
--   column t.x; Postgres names the column after what it casts, so A sorts by the cast
-- witness: t = {(10), (9)}: A returns '10' (the least text), B returns '9' (the least integer)
create table "t" ("x" INTEGER);
SELECT "x"::text FROM "t" ORDER BY "x" LIMIT 1;
SELECT "x"::text FROM "t" ORDER BY "t"."x" LIMIT 1;
