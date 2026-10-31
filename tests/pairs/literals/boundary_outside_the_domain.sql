-- Unless explicitly stated otherwise all files in this repository are licensed under the
-- Apache License Version 2.0.
-- This product includes software developed at Datadog (https://www.datadoghq.com/).
-- Copyright 2026-Present Datadog, Inc.

-- truth: not-equivalent
-- expect frontend: emit
-- expect fuzz: counterexample
-- expect qed: no-proof
-- expect sqleq-solver: no-proof
-- expect sqlsolver-jvm: no-proof
-- expect cvc5: model
-- expect lean: unsupported
-- origin: issue #187: no generated row equals a literal outside the value domain, so a pair that
--   differs only at the literal's boundary agreed on every trial
-- witness: t = {(334)}: A returns 334, B returns no row
CREATE TABLE t (c int8 NOT NULL);
SELECT * FROM t WHERE 334 <= c;
SELECT * FROM t WHERE 334 < c;
