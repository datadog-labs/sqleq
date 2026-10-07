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
-- origin: INSERT ... ON CONFLICT is refused, not lowered (docs/SOUNDNESS.md); a pin for each
--   documented refusal (#68)
-- witness: t = {(1, 0)}: A leaves (1, 5), B leaves (1, 6)
create table "t" ("id" INTEGER, "a" INTEGER, unique ("id"));
INSERT INTO t (id, a) VALUES (1, 1) ON CONFLICT (id) DO UPDATE SET a = 5;
INSERT INTO t (id, a) VALUES (1, 1) ON CONFLICT (id) DO UPDATE SET a = 6;
