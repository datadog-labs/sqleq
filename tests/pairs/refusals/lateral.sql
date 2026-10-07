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
-- origin: a LATERAL derived table is refused, not lowered (docs/SOUNDNESS.md); a pin for each
--   documented refusal (#68)
-- witness: t = {(1, 1)}; u = {(1, 1), (2, 1)}: A takes one match and returns 1, B returns 1 twice
create table "t" ("id" INTEGER, "a" INTEGER);
create table "u" ("id" INTEGER, "a" INTEGER);
SELECT t.id FROM t, LATERAL (SELECT u.id FROM u WHERE u.a = t.a LIMIT 1) AS l;
SELECT t.id FROM t JOIN u ON u.a = t.a;
