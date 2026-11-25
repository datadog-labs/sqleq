-- Unless explicitly stated otherwise all files in this repository are licensed under the
-- Apache License Version 2.0.
-- This product includes software developed at Datadog (https://www.datadoghq.com/).
-- Copyright 2026-Present Datadog, Inc.

-- truth: equivalent
-- expect frontend: emit-reflexive
-- expect fuzz: no-counterexample
-- expect qed: proved-literal
-- expect sqleq-solver: proved-literal
-- expect cvc5: proved-literal
-- expect lean: unsupported
-- catalog: inferred-seeded
-- origin: #302's review, the positive side: a comparison with a number constant is a use the rule reads
-- argument: $1 = 5 makes $1 an integer, as id = $1 does, so $1 is an integer in both statements and the cast is the
--   identity
CREATE TABLE t (id int PRIMARY KEY, a int, b int, s text, d date, d2 date, ts timestamp, lo timestamp, hi timestamp);
SELECT id FROM t WHERE $1::int = 5 AND id = $1;
SELECT id FROM t WHERE $1 = 5 AND id = $1;
