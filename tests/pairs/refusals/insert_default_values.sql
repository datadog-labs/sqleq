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
-- origin: INSERT ... DEFAULT VALUES is refused, not lowered (docs/SOUNDNESS.md); a pin for each
--   documented refusal (#68)
-- witness: t = {}: A adds (NULL, 0), B adds (NULL, NULL)
create table "t" ("id" INTEGER, "a" INTEGER DEFAULT 0);
INSERT INTO t DEFAULT VALUES;
INSERT INTO t (id, a) VALUES (NULL, NULL);
