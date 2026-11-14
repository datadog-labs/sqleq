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
-- origin: issue #207: sqleq-fuzz read a literal under a cast as the text inside it. This cast is the text
--   1.7976931348623157e+308, below which neither 'a', 'b', 'c' nor the text inside the cast sorts. The check keeps
--   out the empty string, which is below it too and is drawn for its own reason
-- witness: t = {('1.7976931348623157e+308')}: the cast is that text, so A returns the row and B returns none
CREATE TABLE t (c text NOT NULL CHECK (c <> ''));
SELECT * FROM t WHERE CAST('1.7976931348623157e308'::float8 AS text) >= c;
SELECT * FROM t WHERE false;
