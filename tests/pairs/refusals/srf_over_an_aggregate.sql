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
-- origin: a set-returning function over an aggregate is refused, not lowered (docs/SOUNDNESS.md); a
--   pin for each documented refusal (#68)
-- witness: t = {(1, 2)}: A returns 1 and 2, B returns 2
create table "t" ("id" INTEGER, "a" INTEGER);
SELECT generate_series(1, max(a)) FROM t;
SELECT max(a) FROM t;
