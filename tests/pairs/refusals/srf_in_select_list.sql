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
-- origin: a set-returning function in the SELECT list is refused, not lowered (docs/SOUNDNESS.md);
--   a pin for each documented refusal (#68)
-- witness: t = {(1, 2)}: A returns (1, 1) and (1, 2), B returns (1, 2)
create table "t" ("id" INTEGER, "a" INTEGER);
SELECT id, generate_series(1, a) FROM t;
SELECT id, a FROM t;
