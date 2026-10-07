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
-- origin: FETCH ... WITH TIES is refused, not lowered (docs/SOUNDNESS.md); a pin for each
--   documented refusal (#68)
-- witness: t = {(1, 1), (2, 1)}: A returns both tied rows, B one of them
create table "t" ("id" INTEGER, "a" INTEGER);
SELECT id FROM t ORDER BY a FETCH FIRST 1 ROW WITH TIES;
SELECT id FROM t ORDER BY a LIMIT 1;
