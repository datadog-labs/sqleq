-- Unless explicitly stated otherwise all files in this repository are licensed under the
-- Apache License Version 2.0.
-- This product includes software developed at Datadog (https://www.datadoghq.com/).
-- Copyright 2026-Present Datadog, Inc.

-- truth: not-equivalent
-- expect frontend: refuse:schema
-- expect fuzz: counterexample
-- expect qed: no-plan
-- expect sqleq-solver: no-plan
-- expect lean: unsupported
-- origin: a correlated column it cannot resolve (here, in a VALUES list) is refused, not lowered
--   (docs/SOUNDNESS.md); a pin for each documented refusal (#68)
-- witness: t = {(1, 1, 2)}: A tests 1 IN (2, 1) and returns 1, B tests 1 = 2 and returns no rows
create table "t" ("id" INTEGER, "a" INTEGER, "b" INTEGER);
SELECT id FROM t WHERE a IN (VALUES (t.b), (1));
SELECT id FROM t WHERE a = b;
