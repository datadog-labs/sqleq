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
-- origin: a window function is refused, not lowered (docs/SOUNDNESS.md); a pin for each documented
--   refusal (#68)
-- witness: t = {(1, 1), (2, 1)}: A numbers the tie 1 and 2, B ranks both 1
create table "t" ("id" INTEGER, "a" INTEGER);
SELECT a, row_number() OVER (ORDER BY a) FROM t;
SELECT a, rank() OVER (ORDER BY a) FROM t;
