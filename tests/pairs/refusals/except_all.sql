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
-- origin: EXCEPT ALL is refused, not lowered (docs/SOUNDNESS.md); a pin for each documented refusal
--   (#68)
-- witness: t = {(1), (1)}; u = {(1)}: A removes one copy and returns 1, B removes both and returns
--   no rows
create table "t" ("a" INTEGER);
create table "u" ("a" INTEGER);
SELECT a FROM t EXCEPT ALL SELECT a FROM u;
SELECT a FROM t EXCEPT SELECT a FROM u;
