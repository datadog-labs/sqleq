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
-- origin: a set-returning function in FROM is refused, not lowered (docs/SOUNDNESS.md); a pin for
--   each documented refusal (#68)
-- witness: t = {(1, 0)}: A returns 1 twice, B once
create table "t" ("id" INTEGER, "a" INTEGER);
SELECT t.id FROM t, generate_series(1, 2) AS g;
SELECT t.id FROM t;
