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
-- origin: a pair of INSERTs whose RETURNING lists differ is refused, not lowered
--   (docs/SOUNDNESS.md); a pin for each documented refusal (#68)
-- witness: t = {}: both add (1, 2); A returns 1, B returns 2
create table "t" ("id" INTEGER, "a" INTEGER);
INSERT INTO t (id, a) VALUES (1, 2) RETURNING id;
INSERT INTO t (id, a) VALUES (1, 2) RETURNING a;
