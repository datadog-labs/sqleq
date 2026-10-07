-- Unless explicitly stated otherwise all files in this repository are licensed under the
-- Apache License Version 2.0.
-- This product includes software developed at Datadog (https://www.datadoghq.com/).
-- Copyright 2026-Present Datadog, Inc.

-- truth: not-equivalent
-- expect frontend: emit
-- expect fuzz: no-counterexample
-- expect qed: no-proof
-- expect sqleq-solver: unsupported
-- expect lean: unsupported
-- origin: a row slice is lowered as a sort, not refused (docs/SOUNDNESS.md), so a pair that slices
--   in two orders must not be proved (#68)
-- witness: t = {(1, 1), (2, 2)}: A returns 1, B returns 2
create table "t" ("id" INTEGER, "a" INTEGER);
SELECT id FROM t ORDER BY a LIMIT 1;
SELECT id FROM t ORDER BY a DESC LIMIT 1;
