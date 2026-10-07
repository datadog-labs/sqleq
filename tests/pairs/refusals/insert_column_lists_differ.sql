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
-- origin: a pair of INSERTs whose column lists differ is refused, not lowered (docs/SOUNDNESS.md);
--   a pin for each documented refusal (#68)
-- witness: t = {}: A adds (1, 2), B adds (2, 1)
create table "t" ("id" INTEGER, "a" INTEGER);
INSERT INTO t (id, a) VALUES (1, 2);
INSERT INTO t (a, id) VALUES (1, 2);
