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
-- origin: issue #207: sqleq-fuzz's string domain was 'a', 'b' and 'c', so a predicate only the empty string
--   satisfies held of no generated row
-- witness: t = {(1, '')}: length('') is 0, so A returns 1 and B returns no row
CREATE TABLE t (id int PRIMARY KEY, c text);
SELECT id FROM t WHERE length(c) = 0;
SELECT id FROM t WHERE false;
