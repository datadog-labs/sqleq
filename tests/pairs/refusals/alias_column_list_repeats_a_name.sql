-- Unless explicitly stated otherwise all files in this repository are licensed under the
-- Apache License Version 2.0.
-- This product includes software developed at Datadog (https://www.datadoghq.com/).
-- Copyright 2026-Present Datadog, Inc.

-- truth: not-equivalent
-- expect frontend: refuse:schema
-- expect fuzz: counterexample
-- expect qed: no-plan
-- expect sqleq-solver: no-plan
-- expect lean: unsupported
-- origin: an alias column list that leaves two columns with one name is refused, not lowered
--   (docs/SOUNDNESS.md); a pin for each documented refusal (#68)
-- witness: t = {(1, 2, 3)}: x(c, a) names t's columns c, a, c, so A's x.a is t.b and returns 2; B
--   returns 1
create table "t" ("a" INTEGER, "b" INTEGER, "c" INTEGER);
SELECT x.a FROM t AS x(c, a);
SELECT x.a FROM t AS x;
