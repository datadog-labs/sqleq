-- Unless explicitly stated otherwise all files in this repository are licensed under the
-- Apache License Version 2.0.
-- This product includes software developed at Datadog (https://www.datadoghq.com/).
-- Copyright 2026-Present Datadog, Inc.

-- truth: not-equivalent
-- expect frontend: refuse:unsupported
-- expect fuzz: error
-- expect qed: no-plan
-- expect sqleq-solver: no-plan
-- expect lean: unsupported
-- origin: TABLESAMPLE is refused, not lowered (docs/SOUNDNESS.md); a pin for each documented
--   refusal (#68)
-- witness: t = {(1)}: a zero-percent sample is empty, so A returns no rows and B returns 1
create table "t" ("id" INTEGER);
SELECT id FROM t TABLESAMPLE BERNOULLI (0);
SELECT id FROM t;
