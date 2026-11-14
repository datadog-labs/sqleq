-- Unless explicitly stated otherwise all files in this repository are licensed under the
-- Apache License Version 2.0.
-- This product includes software developed at Datadog (https://www.datadoghq.com/).
-- Copyright 2026-Present Datadog, Inc.

-- truth: equivalent
-- expect frontend: emit
-- expect fuzz: no-counterexample
-- expect qed: no-proof
-- expect sqleq-solver: no-proof
-- expect cvc5: no-proof
-- expect lean: unsupported
-- origin: issue #207, the negative side: the literal is drawn into the column under the expression, and the
--   expression never takes it, so the two stay indistinguishable
-- argument: abs(c) is never negative, so neither abs(c) <= -2 nor abs(c) < -2 holds of any row, and both
--   return none
CREATE TABLE t (c int4);
SELECT * FROM t WHERE abs(c) <= -2;
SELECT * FROM t WHERE abs(c) < -2;
