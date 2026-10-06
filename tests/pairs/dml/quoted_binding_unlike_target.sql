-- Unless explicitly stated otherwise all files in this repository are licensed under the
-- Apache License Version 2.0.
-- This product includes software developed at Datadog (https://www.datadoghq.com/).
-- Copyright 2026-Present Datadog, Inc.

-- truth: not-equivalent
-- expect frontend: emit
-- expect fuzz: error
-- expect qed: no-proof
-- expect sqleq-solver: no-proof
-- expect lean: unsupported
-- origin: issue #57: the DML shadow check folds quoted names correctly and lets WITH "T" pass, but
--   inline_ctes folded "T" to t and replaced the DELETE target with the binding
-- witness: t = {(1, 2), (2, 1)}: A deletes both rows, B deletes (1, 2) only

create table "t" ("a" INTEGER, "b" INTEGER);
WITH "T" AS (SELECT * FROM t WHERE a = 1) DELETE FROM t;
DELETE FROM t WHERE a = 1;
