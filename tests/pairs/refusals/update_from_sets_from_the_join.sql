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
-- origin: an UPDATE ... FROM whose SET reads the FROM list is refused, not lowered
--   (docs/SOUNDNESS.md); a pin for each documented refusal (#68)
-- witness: t = {(1, 1)}; u = {(5, 1)}: A leaves (1, 5), B leaves (1, 1)
create table "t" ("id" INTEGER, "a" INTEGER);
create table "u" ("id" INTEGER, "a" INTEGER);
UPDATE t SET a = u.id FROM u WHERE t.a = u.a;
UPDATE t SET a = 1 WHERE EXISTS (SELECT 1 FROM u WHERE t.a = u.a);
