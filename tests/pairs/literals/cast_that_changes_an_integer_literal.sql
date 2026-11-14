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
-- origin: issue #207: sqleq-fuzz read a literal under a cast as the text inside it. This cast is 0, and the
--   text, -1.7305e-12, is no integer, so the column took no value from it and no generated row was below 0
-- witness: t = {(-1)}: -1 < 0, so A returns the row and B returns none
CREATE TABLE t (c int8);
SELECT * FROM t WHERE c < CAST('-1.7305e-12'::float8 AS int8);
SELECT * FROM t WHERE false;
