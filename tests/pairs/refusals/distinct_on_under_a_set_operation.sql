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
-- origin: DISTINCT ON under a set operation is refused, not lowered (docs/SOUNDNESS.md); a pin for
--   each documented refusal (#68)
-- witness: t = {(1, 1), (2, 1)}: A keeps one row of the first branch and returns three rows, B
--   returns four
create table "t" ("id" INTEGER, "a" INTEGER);
SELECT DISTINCT ON (a) a, id FROM t UNION ALL SELECT a, id FROM t;
SELECT a, id FROM t UNION ALL SELECT a, id FROM t;
