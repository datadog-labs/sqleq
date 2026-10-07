-- Unless explicitly stated otherwise all files in this repository are licensed under the
-- Apache License Version 2.0.
-- This product includes software developed at Datadog (https://www.datadoghq.com/).
-- Copyright 2026-Present Datadog, Inc.

-- truth: not-equivalent
-- expect frontend: refuse:unsupported
-- expect fuzz: no-counterexample
-- expect qed: no-plan
-- expect sqleq-solver: no-plan
-- expect lean: unsupported
-- origin: an INSERT that omits a column whose default is nextval() is refused, not lowered
--   (docs/SOUNDNESS.md); a pin for each documented refusal (#68)
-- witness: t = {}, the sequence fresh: A adds (1, 1) and (2, 2), B adds (1, 2) and (2, 1)
create table "t" ("id" SERIAL, "a" INTEGER);
INSERT INTO t (a) VALUES (1), (2);
INSERT INTO t (a) VALUES (2), (1);
