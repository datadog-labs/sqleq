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
-- origin: LIKE ... ESCAPE is refused, not lowered (docs/SOUNDNESS.md); a pin for each documented
--   refusal (#68)
-- witness: t = {(1, 'ab')}: A matches only the string 'a%' and returns no rows, B returns 1
create table "t" ("id" INTEGER, "s" VARCHAR);
SELECT id FROM t WHERE s LIKE 'a!%' ESCAPE '!';
SELECT id FROM t WHERE s LIKE 'a%';
