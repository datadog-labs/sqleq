-- Unless explicitly stated otherwise all files in this repository are licensed under the
-- Apache License Version 2.0.
-- This product includes software developed at Datadog (https://www.datadoghq.com/).
-- Copyright 2026-Present Datadog, Inc.

-- truth: not-equivalent
-- expect frontend: refuse:unsupported
-- expect fuzz: counterexample
-- expect qed: no-plan
-- expect sqleq-solver: no-plan
-- expect cvc5: no-plan
-- expect lean: unsupported
-- origin: PR #209 review: a table aliased __sqleq_window, the RANK() rewrite's own alias, was compared with
--   itself and never sorted before itself, so every rank was 1; the reserved name is refused
-- witness: t = {(1), (2)}: A returns (1, 1), (2, 2); B returns (1, 1), (2, 1)
create table t (b integer);
SELECT b, RANK() OVER (ORDER BY b) AS r FROM t AS __sqleq_window;
SELECT b, 1 AS r FROM t;
