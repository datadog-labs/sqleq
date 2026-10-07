-- Unless explicitly stated otherwise all files in this repository are licensed under the
-- Apache License Version 2.0.
-- This product includes software developed at Datadog (https://www.datadoghq.com/).
-- Copyright 2026-Present Datadog, Inc.

-- truth: not-equivalent
-- expect frontend: emit
-- expect fuzz: counterexample
-- expect qed: no-proof
-- expect sqleq-solver: unsupported
-- expect lean: unsupported
-- origin: DISTINCT ON is lowered, not refused (docs/SOUNDNESS.md), so a pair that keeps a different
--   row per key must not be proved (#68)
-- witness: t = {(1, 1), (1, 2)}: A keeps (1, 1), B keeps (1, 2)
create table "t" ("a" INTEGER, "b" INTEGER);
SELECT DISTINCT ON (a) a, b FROM t ORDER BY a, b;
SELECT DISTINCT ON (a) a, b FROM t ORDER BY a, b DESC;
