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
-- origin: issue #56: an exponent literal with no . was typed INTEGER, and sqleq-solver truncated
--   1e-5 to 0
-- witness: t = {(0)}: A yields no rows (0 = 0.00001 is false), B yields (0)
create table "t" ("a" INTEGER);
SELECT "a" FROM "t" WHERE "a" = 1e-5;
SELECT "a" FROM "t" WHERE "a" = 0;
