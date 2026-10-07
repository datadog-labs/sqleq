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
-- origin: unnest in the SELECT list is refused, not lowered (docs/SOUNDNESS.md); a pin for each
--   documented refusal (#68)
-- witness: t = {(1, '{5,6}')}: A returns (1, 5) and (1, 6), B returns (1, 5)
create table "t" ("id" INTEGER, "arr" INTEGER[]);
SELECT id, unnest(arr) FROM t;
SELECT id, arr[1] FROM t;
