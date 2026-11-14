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
-- origin: issue #207: the literal is compared with a CASE whose result is the column, so no generated row was
--   at or below it
-- witness: t = {(-2)}: the CASE is -2, which is not above -2, so A returns the row and B returns none
CREATE TABLE t (c int4);
SELECT * FROM t WHERE NOT ((CASE WHEN false THEN 0 ELSE c END) > -2);
SELECT * FROM t WHERE false;
