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
-- origin: issue #55: integer / reached the QED prover as z3's Euclidean div, which rounds -1 / 2 to
--   -1 where Postgres truncates it to 0
-- witness: t = {(-1)}: A yields no rows (-1 / 2 = 0 in Postgres, and 0 > -1), B yields (-1)
create table "t" ("a" INTEGER);
SELECT "a" FROM "t" WHERE "a" / 2 * 2 <= "a";
SELECT "a" FROM "t" WHERE "a" IS NOT NULL;
