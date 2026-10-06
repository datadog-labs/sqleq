-- Unless explicitly stated otherwise all files in this repository are licensed under the
-- Apache License Version 2.0.
-- This product includes software developed at Datadog (https://www.datadoghq.com/).
-- Copyright 2026-Present Datadog, Inc.

-- truth: not-equivalent
-- expect frontend: emit
-- expect fuzz: no-counterexample
-- expect qed: no-proof
-- expect sqleq-solver: no-proof
-- expect lean: unsupported
-- origin: issue #56: an integer literal past the bigint range was typed INTEGER, and sqleq-solver
--   saturated it to the largest bigint
-- witness: t = {(9223372036854775807)}: A yields no rows (the literal is one past the bigint
--   range), B yields the row
create table "t" ("b" BIGINT);
SELECT "b" FROM "t" WHERE "b" = 9223372036854775808;
SELECT "b" FROM "t" WHERE "b" = 9223372036854775807;
