-- Unless explicitly stated otherwise all files in this repository are licensed under the
-- Apache License Version 2.0.
-- This product includes software developed at Datadog (https://www.datadoghq.com/).
-- Copyright 2026-Present Datadog, Inc.

-- truth: not-equivalent
-- expect frontend: emit
-- expect fuzz: counterexample
-- expect qed: no-proof
-- expect sqleq-solver: no-proof
-- expect cvc5: no-proof
-- expect lean: unsupported
-- origin: issue #207: a simple CASE holds its operand against each WHEN arm, which is a comparison with a
--   literal that sqleq-fuzz did not read as one
-- witness: t = {('2001-01-13')}: the CASE is 1 on that row, so A returns it and B returns none
CREATE TABLE t (c date);
SELECT * FROM t WHERE (CASE c WHEN '2001-01-13'::date THEN 1 END) = 1;
SELECT * FROM t WHERE false;
