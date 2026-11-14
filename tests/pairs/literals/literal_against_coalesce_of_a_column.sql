-- Unless explicitly stated otherwise all files in this repository are licensed under the
-- Apache License Version 2.0.
-- This product includes software developed at Datadog (https://www.datadoghq.com/).
-- Copyright 2026-Present Datadog, Inc.

-- truth: not-equivalent
-- expect frontend: emit
-- expect fuzz: counterexample
-- expect qed: no-proof
-- expect sqleq-solver: no-proof
-- expect cvc5: model
-- expect lean: unsupported
-- origin: issue #207: sqleq-fuzz drew a literal into a column only where the pair compares the bare column with
--   it, so a boundary on an expression of the column, coalesce(c, 0), was met by no generated row
-- witness: t = {(-2)}: coalesce(-2, 0) <= -2 holds and coalesce(-2, 0) < -2 does not, so A returns the row and
--   B returns none
CREATE TABLE t (c int4);
SELECT * FROM t WHERE coalesce(c, 0) <= -2;
SELECT * FROM t WHERE coalesce(c, 0) < -2;
